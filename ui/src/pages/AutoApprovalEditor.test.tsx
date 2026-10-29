import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router'
import { describe, expect, it, vi } from 'vitest'

import AutoApprovalEditor from './AutoApprovalEditor'
import { api } from '../lib/api'
import { type RulesPage } from '../lib/approvalRules'
import { parseValue } from '../lib/autoApproval'
import { SessionContext, type SessionState } from '../lib/session'

const gbp = (amount: number) => ({
  amount,
  currency: 'GBP',
  minor_units: 2,
  display: `£${(amount / 100).toFixed(2)}`,
  figure: amount % 100 ? (amount / 100).toFixed(2) : String(amount / 100),
})

/** One money operation, as the rules page lists it. */
const rulesPage: RulesPage = {
  operations: [
    {
      skill_id: 'sk1',
      skill_name: 'Bookings',
      requires: 'charge',
      matches: 'POST /charges',
      name: 'Charge',
      agents: [],
      amount: { field: 'amount_pence', currency: 'GBP', minor_units: 2, standard_minor_units: 2 },
      fixed: { never: false, beyond_deployment: false, most: gbp(500000) },
      rules: [],
      asked: 0,
      approved: 0,
      declined: 0,
    },
  ],
  days: 30,
  may_write: true,
}

/** A policy for low-risk charges, in trial. */
const policy = {
  id: 'p1',
  agent_id: null,
  agent_name: null,
  effect: 'include',
  selector: 'risk',
  max_risk: 'low',
  requires: null,
  skill_id: null,
  skill_name: null,
  matches: null,
  conditions: [{ field: 'amount_pence', op: 'le', value: 5000 }],
  per_turn: 1,
  per_day: 10,
  sum_field: null,
  sum_per_day: null,
  daily_totals: { GBP: 15000 },
  daily_totals_money: { GBP: gbp(15000) },
  mode: 'trial',
  version: 1,
  note: null,
  created_by_name: 'Tester',
  created_at: '2026-10-01T00:00:00Z',
  updated_by_name: null,
  updated_at: '2026-10-01T00:00:00Z',
  expires_at: '2026-12-01T00:00:00Z',
  revoked_by_name: null,
  revoked_at: null,
  auto_approved: 0,
  state: 'trial',
}

vi.mock('../lib/api', async (actual) => ({
  ...(await actual<typeof import('../lib/api')>()),
  allPages: vi.fn(async () => []),
  api: vi.fn(async () => null),
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
          initialEntries={['/settings/auto-approval/new?requires=charge&selector=operation&field=booking_id&field=amount_pence']}
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

  it('shows a daily total as money and saves it in minor units', async () => {
    const calls: { path: string; body?: unknown }[] = []
    vi.mocked(api).mockImplementation(async (path: string, init?: RequestInit) => {
      calls.push({ path, body: init?.body ? JSON.parse(String(init.body)) : undefined })
      if (path === '/v1/approval-rules') return rulesPage as never
      if (path.endsWith('/evidence')) return null as never
      return policy as never
    })
    render(
      <SessionContext.Provider value={admin}>
        <MemoryRouter initialEntries={['/settings/auto-approval/p1']}>
          <Routes>
            <Route path="/settings/auto-approval/:id" element={<AutoApprovalEditor />} />
          </Routes>
        </MemoryRouter>
      </SessionContext.Provider>,
    )
    expect(await screen.findByLabelText('Daily total 1')).toHaveProperty('value', '150')
    expect(screen.getByLabelText('Currency 1')).toHaveProperty('value', 'GBP')
    expect(screen.getByText('in whole GBP')).toBeTruthy()
    // The amount field's test is in its own units, and says which beside it.
    expect(screen.getByText('in hundredths of GBP')).toBeTruthy()
    expect(screen.getByLabelText(/^Highest risk/)).toHaveProperty('value', 'low')

    await userEvent.clear(screen.getByLabelText('Daily total 1'))
    await userEvent.type(screen.getByLabelText('Daily total 1'), '200.50')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(calls.some((c) => c.path === '/v1/approval-policies/p1' && c.body)).toBe(true))
    const saved = calls.find((c) => c.path === '/v1/approval-policies/p1' && c.body)?.body as Record<string, unknown>
    expect(saved.daily_totals).toEqual({ GBP: 20050 })
    expect(saved.max_risk).toBe('low')
  })

  it('reads a whole number as a number and quoted text as text', () => {
    expect(parseValue('le', '5000')).toBe(5000)
    expect(parseValue('eq', '"5000"')).toBe('5000')
    expect(parseValue('eq', 'spam')).toBe('spam')
    expect(parseValue('in', 'spam, junk, 3')).toEqual(['spam', 'junk', 3])
  })

  it('says why the record would still have asked', async () => {
    vi.mocked(api).mockImplementation(async (path: string) => {
      if (path === '/v1/approval-rules') return rulesPage as never
      if (path.endsWith('/evidence'))
        return {
          auto_approved: 0,
          auto_approved_week: 0,
          trial_approved: 0,
          trial_declined: 0,
          trial_pending: 0,
          history: [
            {
              item_id: 'i1',
              at: '2026-10-20T00:00:00Z',
              requires: 'charge',
              binds: [{ field: 'amount_pence', value: 29000 }],
              answer: 'approved',
              would: false,
              why: '£290.00 is medium risk',
            },
          ],
        } as never
      return policy as never
    })
    render(
      <SessionContext.Provider value={admin}>
        <MemoryRouter initialEntries={['/settings/auto-approval/p1']}>
          <Routes>
            <Route path="/settings/auto-approval/:id" element={<AutoApprovalEditor />} />
          </Routes>
        </MemoryRouter>
      </SessionContext.Provider>,
    )
    expect(await screen.findByText(/this would still have asked: £290.00 is medium risk/)).toBeTruthy()
  })
})
