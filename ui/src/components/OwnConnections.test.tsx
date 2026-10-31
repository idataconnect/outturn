import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import OwnConnections from './OwnConnections'

/** The test key the Rust side pins (`egress::seal` tests), so a seal made
 *  here is one the gateway's tests could open. */
const PUBLIC_KEY = '13be4feaeaf204c7fd3358fc9c00721881d174278128227ec674f37f7fe97b6d'

const sent: { path: string; method: string; body: unknown }[] = []
let listed: unknown

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api')
  return {
    ...actual,
    api: vi.fn(async (path: string, init?: RequestInit) => {
      const method = init?.method ?? 'GET'
      if (method === 'GET') return listed
      sent.push({ path, method, body: init?.body ? JSON.parse(init.body as string) : null })
      return undefined
    }),
  }
})

const books = {
  rule_id: 'r1',
  host: 'books.example.com',
  header: 'authorization',
  connected: null,
}

describe('OwnConnections', () => {
  beforeEach(() => {
    sent.length = 0
  })

  it('shows nothing where no connection is anybody’s own', async () => {
    listed = { connections: [], seal_key: { public_key: PUBLIC_KEY, key_id: 'k1' } }
    const { container } = render(<OwnConnections workspaceId="w1" userId="u1" />)
    await waitFor(() => expect(container).toBeEmptyDOMElement())
  })

  it('seals a key as the person’s own, for the host’s header, and never sends it bare', async () => {
    listed = { connections: [books], seal_key: { public_key: PUBLIC_KEY, key_id: 'k1' } }
    render(<OwnConnections workspaceId="w1" userId="u1" />)
    fireEvent.click(await screen.findByRole('button', { name: 'Connect your key' }))
    fireEvent.change(screen.getByLabelText(/Your key/), { target: { value: 'Bearer dana-secret' } })
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }))

    await waitFor(() => expect(sent).toHaveLength(1))
    const [put] = sent
    expect(put.path).toBe('/v1/connections/r1')
    expect(put.method).toBe('PUT')
    expect(JSON.stringify(put.body)).not.toContain('dana-secret')
    const body = put.body as { id: string; binding: string; key_id: string }
    expect(body.key_id).toBe('k1')
    expect(JSON.parse(atob(body.binding))).toEqual({
      credential: body.id,
      kind: 'static',
      workspaces: ['w1'],
      hosts: ['books.example.com'],
      header: 'authorization',
      user: 'u1',
    })
  })

  it('disconnects the person’s own key', async () => {
    listed = {
      connections: [{ ...books, connected: { credential_id: 'c1', connected_at: '2026-10-31T00:00:00Z' } }],
      seal_key: { public_key: PUBLIC_KEY, key_id: 'k1' },
    }
    vi.spyOn(window, 'confirm').mockReturnValue(true)
    render(<OwnConnections workspaceId="w1" userId="u1" />)
    fireEvent.click(await screen.findByRole('button', { name: 'Disconnect' }))
    await waitFor(() => expect(sent).toEqual([{ path: '/v1/connections/r1', method: 'DELETE', body: null }]))
  })
})
