import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { Connection } from './SkillConnections'
import type { EgressRule } from '../lib/credentials'

const sent: { path: string; method: string; body: unknown }[] = []

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api')
  return {
    ...actual,
    api: vi.fn(async (path: string, init?: RequestInit) => {
      sent.push({
        path,
        method: init?.method ?? 'GET',
        body: init?.body ? JSON.parse(init.body as string) : null,
      })
      return undefined
    }),
  }
})

const books: EgressRule = {
  id: 'r1',
  host: 'books.example.com',
  owner: 'agent',
  header: 'authorization',
  credential_env: null,
  credential: 'c1',
  on_behalf_of: false,
  enabled: true,
}

function jwks(keys: unknown[]) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => new Response(JSON.stringify({ keys }), { status: 200 })),
  )
}

describe('Connection, saying who is asking', () => {
  beforeEach(() => {
    sent.length = 0
  })
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('turns it on for a host the agent has a key for', async () => {
    jwks([])
    const onChanged = vi.fn(async () => {})
    render(
      <Connection rule={books} credential={null} name="books" workspaceId="w1" onChanged={onChanged} />,
    )
    fireEvent.click(screen.getByRole('checkbox', { name: /Tell it who is asking/ }))
    await waitFor(() =>
      expect(sent).toEqual([
        { path: '/v1/egress-rules/r1/on-behalf-of', method: 'PUT', body: { on_behalf_of: true } },
      ]),
    )
    await waitFor(() => expect(onChanged).toHaveBeenCalled())
  })

  it('warns when the deployment publishes no key to check it with', async () => {
    jwks([])
    render(
      <Connection
        rule={{ ...books, on_behalf_of: true }}
        credential={null}
        name="books"
        workspaceId="w1"
        onChanged={async () => {}}
      />,
    )
    expect(await screen.findByRole('status')).toHaveTextContent(/publishes no signing key/)
  })

  it('says nothing more when a key is published', async () => {
    jwks([{ kty: 'OKP', crv: 'Ed25519', x: 'x', kid: 'k' }])
    render(
      <Connection
        rule={{ ...books, on_behalf_of: true }}
        credential={null}
        name="books"
        workspaceId="w1"
        onChanged={async () => {}}
      />,
    )
    await waitFor(() => expect(fetch).toHaveBeenCalledWith('/.well-known/jwks.json'))
    expect(screen.queryByText(/publishes no signing key/)).toBeNull()
  })

  it('is not offered where each person connects their own key', () => {
    jwks([])
    render(
      <Connection
        rule={{ ...books, owner: 'asker' }}
        credential={null}
        name="books"
        workspaceId="w1"
        onChanged={async () => {}}
      />,
    )
    expect(screen.queryByRole('checkbox', { name: /Tell it who is asking/ })).toBeNull()
  })
})
