import { render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { describe, expect, it, vi } from 'vitest'

import AutoApprovalEditor from './AutoApprovalEditor'
import { parseValue } from '../lib/autoApproval'
import { SessionContext, type SessionState } from '../lib/session'

vi.mock('../lib/api', async (actual) => ({
  ...(await actual<typeof import('../lib/api')>()),
  allPages: vi.fn(async () => []),
  api: vi.fn(),
}))

const admin: SessionState = {
  status: 'authenticated',
  displayName: 'Tester',
  workspaces: [],
  session: {
    session_id: 's',
    workspace_id: 'w',
    display_name: 'Tester',
    roles: [],
    authorities: ['approvals:answer', 'approvals:delegate'],
    workspaces: [],
  },
}

describe('AutoApprovalEditor', () => {
  it('is prefilled from the address an approval links to', () => {
    render(
      <SessionContext.Provider value={admin}>
        <MemoryRouter
          initialEntries={['/settings/auto-approval/new?requires=charge&selector=act&field=booking_id&field=amount_pence']}
        >
          <Routes>
            <Route path="/settings/auto-approval/new" element={<AutoApprovalEditor />} />
          </Routes>
        </MemoryRouter>
      </SessionContext.Provider>,
    )
    expect(screen.getByLabelText(/^Approval kind/)).toHaveProperty('value', 'charge')
    expect(screen.getByLabelText('Field 1')).toHaveProperty('value', 'booking_id')
    expect(screen.getByLabelText('Field 2')).toHaveProperty('value', 'amount_pence')
    expect(screen.getByLabelText('Mode')).toHaveProperty('value', 'trial')
  })

  it('reads a whole number as a number and quoted text as text', () => {
    expect(parseValue('le', '5000')).toBe(5000)
    expect(parseValue('eq', '"5000"')).toBe('5000')
    expect(parseValue('eq', 'spam')).toBe('spam')
    expect(parseValue('in', 'spam, junk, 3')).toEqual(['spam', 'junk', 3])
  })
})
