import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { MemoryRouter } from 'react-router'

import PlatformSetup from './PlatformSetup'

const vendor = (key: string, name: string, protocol: string, base_url: string) => ({
  key,
  name,
  protocol,
  base_url,
  env: `${key.toUpperCase()}_API_KEY`,
})
const ANTHROPIC = vendor('anthropic', 'Anthropic', 'anthropic', 'https://api.anthropic.com')
const GEMINI = vendor('gemini', 'Google Gemini', 'gemini', 'https://generativelanguage.googleapis.com')
const link = (v: typeof ANTHROPIC, model: string) => ({ vendor: v, base_url: v.base_url, model, name: model })

/** The recipes as the API resolves them, trimmed to `with` as it does. */
function recipes(path: string) {
  const query = new URLSearchParams(path.split('?')[1])
  const with_ = query.get('with')
  const kept = (v: typeof ANTHROPIC) => with_ === null || with_.split(',').includes(v.key)
  const recipe = (
    id: string,
    title: string,
    links: ReturnType<typeof link>[],
    small: ReturnType<typeof link>[],
  ) => {
    const vendors = [...links, ...small]
      .map((l) => l.vendor)
      .filter((v, i, all) => all.findIndex((w) => w.key === v.key) === i)
    return {
      id,
      title,
      summary: '',
      vendors: vendors.map((v) => ({ vendor: v, base_url: v.base_url })),
      links: links.filter((l) => kept(l.vendor)),
      small: small.filter((l) => kept(l.vendor)),
      dropped: vendors.filter((v) => !kept(v)),
      untried: false,
      same_model: false,
    }
  }
  return [
    recipe('anthropic', 'Anthropic', [link(ANTHROPIC, 'claude-sonnet-5-5')], [link(ANTHROPIC, 'claude-haiku-5-5')]),
    recipe('medium', 'Medium tier', [link(ANTHROPIC, 'claude-sonnet-5-5'), link(GEMINI, 'gemini-3.8-flash')], []),
  ]
}

const asked: { path: string; body?: unknown }[] = []
const EMPTY = { accounts: [], models: [], chains: [], assignments: {} }
/** What the API says is configured; a test sets it to what a run left. */
let held: unknown = EMPTY

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api')
  return {
    ...actual,
    api: vi.fn(async (path: string, init?: RequestInit) => {
      asked.push({ path, body: init?.body ? JSON.parse(init.body as string) : undefined })
      if (path.startsWith('/v1/platform/recipes')) return recipes(path)
      if (path === '/v1/platform/models' && !init) return held
      if (path === '/v1/platform/catalog') return { models: 1, providers: 1, source: 'shipped' }
      if (path === '/v1/platform/provider-accounts') return { id: 'acct', name: 'x', protocol: 'anthropic' }
      if (path === '/v1/platform/models') return { id: `m${asked.length}` }
      if (path === '/v1/platform/chains') return { id: `c${asked.length}` }
      if (path === '/v1/platform/test-call') return { ok: true, model: 'm', reply: 'hi', elapsed_ms: 1 }
      return undefined
    }),
  }
})

function show() {
  return render(
    <MemoryRouter>
      <PlatformSetup />
    </MemoryRouter>,
  )
}

describe('PlatformSetup', () => {
  beforeEach(() => {
    asked.length = 0
    held = EMPTY
  })

  it('reuses what an earlier run made, rather than a second of each by the same name', async () => {
    held = {
      ...EMPTY,
      accounts: [{ id: 'a1', name: 'Anthropic', protocol: 'anthropic', base_url: ANTHROPIC.base_url }],
      models: [{ id: 'm1', account_id: 'a1', name: 'Claude Sonnet 5.5', model: 'claude-sonnet-5-5' }],
      chains: [{ id: 'c1', name: 'Anthropic', models: ['m1'] }],
    }
    show()
    const button = await screen.findByRole('button', { name: /Set up Anthropic/ })
    await waitFor(() => expect(button).toBeEnabled())
    await userEvent.click(button)
    await screen.findByRole('status')
    const made = asked.filter((a) => a.body !== undefined)
    expect(made.filter((a) => a.path === '/v1/platform/provider-accounts')).toHaveLength(0)
    // The haiku was never made, so it is; the sonnet was, so it is not.
    expect(made.filter((a) => a.path === '/v1/platform/models').map((a) => (a.body as { model: string }).model)).toEqual([
      'claude-haiku-5-5',
    ])
    expect(made.filter((a) => a.path === '/v1/platform/chains/c1')).toHaveLength(1)
    expect(made.filter((a) => a.path === '/v1/platform/chains').map((a) => (a.body as { name: string }).name)).toEqual([
      'Anthropic, titles and summaries',
    ])
  })

  it('sets up a one-provider recipe with its one key', async () => {
    show()
    const button = await screen.findByRole('button', { name: /Set up Anthropic/ })
    expect(button).toBeDisabled()
    // Only the chosen provider's key is asked for.
    expect(screen.getAllByLabelText('Key')).toHaveLength(1)
    await userEvent.type(screen.getByLabelText('Or a variable in the gateway\'s environment'), 'NOT_A_KEY')
    await waitFor(() => expect(button).toBeEnabled())
    await userEvent.click(button)
    await screen.findByRole('status')
    const made = asked.filter((a) => a.body !== undefined)
    expect(made.filter((a) => a.path === '/v1/platform/provider-accounts')).toHaveLength(1)
    expect(made.filter((a) => a.path === '/v1/platform/models').map((a) => (a.body as { model: string }).model)).toEqual([
      'claude-sonnet-5-5',
      'claude-haiku-5-5',
    ])
    const assigned = asked.filter((a) => a.path.startsWith('/v1/platform/chain-assignments/')).map((a) => a.path)
    expect(assigned).toEqual([
      '/v1/platform/chain-assignments/assistant',
      '/v1/platform/chain-assignments/background',
      '/v1/platform/chain-assignments/session-name',
      '/v1/platform/chain-assignments/compaction',
    ])
  })

  it('sets up a two-vendor recipe with one key, and says what was dropped', async () => {
    show()
    await userEvent.click(await screen.findByRole('radio', { name: 'Medium tier' }))
    const button = screen.getByRole('button', { name: /Set up Medium tier/ })
    expect(button).toBeDisabled()
    expect(screen.getByText(/at least one of Anthropic and Google Gemini/)).toBeInTheDocument()
    await userEvent.type(screen.getAllByLabelText('Or a variable in the gateway\'s environment')[0], 'NOT_A_KEY')
    await waitFor(() => expect(button).toBeEnabled())
    expect(
      screen.getByText('Without a key for Google Gemini, this is set up without it: claude-sonnet-5-5, with no fallback.'),
    ).toBeInTheDocument()
    await userEvent.click(button)
    await screen.findByRole('status')
    const made = asked.filter((a) => a.body !== undefined)
    expect(made.filter((a) => a.path === '/v1/platform/provider-accounts')).toHaveLength(1)
    expect(made.filter((a) => a.path === '/v1/platform/models').map((a) => (a.body as { model: string }).model)).toEqual([
      'claude-sonnet-5-5',
    ])
  })
})
