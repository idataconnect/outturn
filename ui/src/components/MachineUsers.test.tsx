import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import MachineUsers from './MachineUsers'

/** The test key the Rust side pins (`egress::seal` tests). */
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

const nightly = {
  id: 'm1',
  display_name: 'Nightly reports',
  agents: ['a1'],
  connections: [
    { rule_id: 'r1', host: 'books.example.com', header: 'authorization', connected: null },
  ],
}

describe('MachineUsers', () => {
  beforeEach(() => {
    sent.length = 0
  })

  it('shows nothing where no host is anybody’s own and none exist', async () => {
    listed = { machine_users: [], seal_key: { public_key: PUBLIC_KEY, key_id: 'k1' } }
    const { container } = render(<MachineUsers workspaceId="w1" askers={0} reloads={0} />)
    await waitFor(() => expect(container).toBeEmptyDOMElement())
  })

  it('seals a key as the machine user’s own and sends it to its route', async () => {
    listed = { machine_users: [nightly], seal_key: { public_key: PUBLIC_KEY, key_id: 'k1' } }
    render(<MachineUsers workspaceId="w1" askers={1} reloads={0} />)
    expect(await screen.findByText('Used by 1 agent')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Connect its key' }))
    fireEvent.change(screen.getByLabelText(/Nightly reports's key/), {
      target: { value: 'Bearer nightly-secret' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }))

    await waitFor(() => expect(sent).toHaveLength(1))
    const [put] = sent
    expect(put.path).toBe('/v1/machine-users/m1/connections/r1')
    expect(JSON.stringify(put.body)).not.toContain('nightly-secret')
    const body = put.body as { id: string; binding: string }
    expect(JSON.parse(atob(body.binding))).toMatchObject({
      credential: body.id,
      hosts: ['books.example.com'],
      user: 'm1',
    })
  })

  it('makes one by name', async () => {
    listed = { machine_users: [], seal_key: null }
    render(<MachineUsers workspaceId="w1" askers={1} reloads={0} />)
    fireEvent.change(await screen.findByLabelText('Machine user name'), {
      target: { value: 'Nightly reports' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Add machine user' }))
    await waitFor(() =>
      expect(sent).toEqual([
        { path: '/v1/machine-users', method: 'POST', body: { display_name: 'Nightly reports' } },
      ]),
    )
  })
})
