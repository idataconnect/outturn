import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import Dashboard from './Dashboard'
import { SessionContext, type SessionState } from '../lib/session'

const asked: string[] = []

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api')
  return {
    ...actual,
    api: vi.fn(async (path: string) => {
      asked.push(path)
      return summary
    }),
  }
})

let summary: unknown

function signedIn(over: { authorities?: string[]; roles?: string[] } = {}): SessionState {
  return {
    status: 'authenticated',
    displayName: 'Ada',
    workspaces: [],
    session: {
      workspace_id: 'w1',
      authorities: over.authorities ?? ['usage:read'],
      roles: over.roles ?? [],
      display_name: 'Ada',
      workspaces: [],
    },
  } as unknown as SessionState
}

function show(state: SessionState) {
  return render(
    <SessionContext.Provider value={state}>
      <Dashboard />
    </SessionContext.Provider>,
  )
}

/** Finds the hero figure, which several panels would otherwise match. */
async function hero(): Promise<HTMLElement> {
  return await screen.findByTitle('1,000')
}

/** A window with two models, one of which nobody measured. */
function fixture() {
  return {
    from: '2026-09-18T00:00:00Z',
    to: '2026-09-20T00:00:00Z',
    totals: {
      calls: 4,
      prompt_tokens: 300,
      completion_tokens: 100,
      cache_read_tokens: 600,
      cache_write_tokens: 0,
      reasoning_tokens: 0,
      sessions: 2,
      agents: 1,
      workspaces: 2,
    },
    daily: [
      {
        at: '2026-09-18T00:00:00Z',
        calls: 1,
        prompt_tokens: 100,
        completion_tokens: 40,
        cache_read_tokens: 200,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
      },
      {
        at: '2026-09-19T00:00:00Z',
        calls: 3,
        prompt_tokens: 200,
        completion_tokens: 60,
        cache_read_tokens: 400,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
      },
    ],
    by_workspace: [{ key: 'w1', label: 'Acme', calls: 4, tokens: 1000 }],
    by_model: [{ key: 'qwen3.5', label: null, calls: 4, tokens: 1000 }],
    by_agent: [{ key: 'a1', label: 'Helper', calls: 4, tokens: 1000 }],
    by_account: [{ key: 'hoa-sunnyvale', label: null, calls: 4, tokens: 1000 }],
    by_source: [
      { key: 'reported', label: null, calls: 3, tokens: 750 },
      { key: 'unknown', label: null, calls: 1, tokens: 250 },
    ],
    by_traffic: [
      { key: 'assistant', label: null, calls: 3, tokens: 900 },
      // The platform's own work, which carries no agent.
      { key: 'session-name', label: null, calls: 1, tokens: 100 },
    ],
  }
}

describe('Dashboard', () => {
  beforeEach(() => {
    asked.length = 0
    summary = fixture()
  })

  it('leads with the window total and names what it covers', async () => {
    show(signedIn())
    // 1,000 tokens across the five kinds, compacted.
    expect(await hero()).toHaveTextContent('1,000')
    expect(screen.getByText(/across 4 model calls/)).toBeInTheDocument()
  })

  it('says how much of the total nobody measured', async () => {
    show(signedIn())
    // A quarter of the tokens came from a row the provider never reported, so
    // the total is not a figure to bill from and the page says so.
    expect(await screen.findByText(/25% of these tokens/)).toBeInTheDocument()
    expect(screen.getByText(/not a figure to bill from/)).toBeInTheDocument()
  })

  it('keeps quiet about provenance when every row was reported', async () => {
    summary = { ...fixture(), by_source: [{ key: 'reported', label: null, calls: 4, tokens: 1000 }] }
    show(signedIn())
    await hero()
    expect(screen.queryByText(/did not report/)).not.toBeInTheDocument()
  })

  it('offers the platform scope to an operator', async () => {
    show(signedIn({ roles: ['system_admin'] }))
    expect(await screen.findByRole('button', { name: 'Platform' })).toBeInTheDocument()
  })

  it('offers it to nobody else', async () => {
    show(signedIn())
    await waitFor(() => expect(asked.length).toBeGreaterThan(0))
    expect(screen.queryByRole('button', { name: 'Platform' })).not.toBeInTheDocument()
  })

  it('asks for every workspace only once the operator says so', async () => {
    show(signedIn({ roles: ['system_admin'] }))
    await hero()
    // The default is the operator's own workspace: a dashboard must not widen
    // itself to the whole platform without being asked.
    expect(asked.every((path) => !path.includes('scope=all'))).toBe(true)

    await userEvent.click(screen.getByRole('button', { name: 'Platform' }))
    await waitFor(() => expect(asked.some((path) => path.includes('scope=all'))).toBe(true))
  })

  it('asks for the window the reader picked', async () => {
    show(signedIn())
    await hero()
    await userEvent.click(screen.getByRole('button', { name: '7 days' }))
    await waitFor(() => expect(asked.length).toBeGreaterThan(1))

    // Seven days back from the next midnight, so today is a whole bucket.
    const last = asked.at(-1) ?? ''
    const from = new Date(decodeURIComponent(/from=([^&]+)/.exec(last)?.[1] ?? ''))
    const to = new Date(decodeURIComponent(/to=([^&]+)/.exec(last)?.[1] ?? ''))
    expect((to.getTime() - from.getTime()) / 86_400_000).toBe(7)
  })

  it('offers the same figures as a table, for a reader who cannot use the chart', async () => {
    show(signedIn())
    await hero()
    await userEvent.click(screen.getByRole('button', { name: 'Table' }))
    expect(await screen.findByRole('table')).toBeInTheDocument()
  })

  it('asks for nothing at all without the authority to read it', async () => {
    show(signedIn({ authorities: [] }))
    expect(screen.getByText(/needs the usage:read authority/)).toBeInTheDocument()
    expect(asked).toHaveLength(0)
  })

  it('says a quiet window is quiet rather than showing a grid of zeros', async () => {
    const quiet = fixture()
    quiet.totals = { ...quiet.totals, calls: 0 }
    summary = quiet
    show(signedIn())
    expect(await screen.findByText('No model calls in this window.')).toBeInTheDocument()
  })
})
