import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { Link, MemoryRouter, Route, Routes } from 'react-router'

import FirstRun from './FirstRun'
import { SessionContext, type SessionState } from '../lib/session'

let assignments: Record<string, string> = {}
let asks = 0

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api')
  return {
    ...actual,
    api: vi.fn(async () => {
      asks++
      return { accounts: [], models: [], chains: [], assignments }
    }),
  }
})

function signedIn(roles: string[]): SessionState {
  return {
    status: 'authenticated',
    displayName: 'Ada',
    workspaces: [],
    session: { workspace_id: 'w1', authorities: [], roles, display_name: 'Ada', workspaces: [] },
  } as unknown as SessionState
}

function show(roles: string[]) {
  return render(
    <MemoryRouter>
      <SessionContext.Provider value={signedIn(roles)}>
        <Routes>
          <Route
            path="/"
            element={
              <FirstRun>
                <p>Dashboard</p>
              </FirstRun>
            }
          />
          <Route
            path="/platform/setup"
            element={
              <>
                <p>Setup</p>
                <Link to="/">Leave</Link>
              </>
            }
          />
        </Routes>
      </SessionContext.Provider>
    </MemoryRouter>,
  )
}

// In this order: the redirect is once a session, and the session is the
// module's as well as storage's.
describe('FirstRun', () => {
  beforeEach(() => {
    asks = 0
  })

  it('leaves somebody who is not the operator on the dashboard', async () => {
    assignments = {}
    show([])
    expect(await screen.findByText('Dashboard')).toBeInTheDocument()
    expect(asks).toBe(0)
  })

  it('leaves the operator on the dashboard when assistant traffic is answered', async () => {
    assignments = { assistant: 'c1' }
    show(['system_admin'])
    await vi.waitFor(() => expect(asks).toBe(1))
    expect(screen.getByText('Dashboard')).toBeInTheDocument()
  })

  it('takes the operator to setup once, and lets them leave', async () => {
    assignments = {}
    show(['system_admin'])
    expect(await screen.findByText('Setup')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('link', { name: 'Leave' }))
    expect(await screen.findByText('Dashboard')).toBeInTheDocument()
    expect(sessionStorage.getItem('outturn.setup-offered')).not.toBeNull()
    expect(asks).toBe(1)
  })
})
