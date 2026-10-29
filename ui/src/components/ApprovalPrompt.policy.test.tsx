import { render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import ApprovalPrompt from './ApprovalPrompt'
import { answeredCount, approvedAmounts } from '../lib/autoApproval'
import { SessionContext, type SessionState } from '../lib/session'

vi.mock('../lib/actions', () => ({
  answerApproval: vi.fn(),
  resendFollowUp: vi.fn(),
  markFollowUpDone: vi.fn(),
}))
vi.mock('../lib/autoApproval', async (actual) => ({
  ...(await actual<typeof import('../lib/autoApproval')>()),
  answeredCount: vi.fn(),
  approvedAmounts: vi.fn(),
}))

const counted = vi.mocked(answeredCount)
const amounts = vi.mocked(approvedAmounts)

const gbp = (amount: number) => ({
  amount,
  currency: 'GBP',
  minor_units: 2,
  display: `£${(amount / 100).toFixed(2)}`,
  figure: amount % 100 ? (amount / 100).toFixed(2) : String(amount / 100),
})

/** What a workspace approved of charges in pounds. */
const approvedInPounds = (approved: number) => ({
  requires: 'charge',
  days: 30,
  currencies: [
    { currency: 'GBP', minor_units: 2, approved, max: gbp(3999), total: gbp(30000), suggested_low: gbp(5000), all_under: gbp(4000) },
  ],
  may_delegate: true,
})

function signedIn(authorities: string[]): SessionState {
  return {
    status: 'authenticated',
    displayName: 'Tester',
    workspaces: [],
    session: {
      session_id: 's',
      workspace_id: 'w',
      display_name: 'Tester',
      roles: [],
      authorities,
      workspaces: [],
    },
  }
}

const show = (authorities: string[], over: Partial<Parameters<typeof ApprovalPrompt>[0]['approval']> = {}) =>
  render(
    <SessionContext.Provider value={signedIn(authorities)}>
      <ApprovalPrompt
        approval={{
          item_id: 'item-1',
          requires: 'charge',
          binds: [
            { field: 'booking_id', value: 'bk_1' },
            { field: 'amount_pence', value: 4000 },
          ],
          ...over,
        }}
        onAnswered={vi.fn()}
      />
    </SessionContext.Provider>,
  )

describe('the offer drawn from what was approved', () => {
  beforeEach(() => {
    counted.mockReset()
    amounts.mockReset()
  })

  it('says how many and how large, and opens the rule at the suggested figure', async () => {
    amounts.mockResolvedValue(approvedInPounds(12))
    show(['approvals:answer', 'approvals:delegate'])
    expect(await screen.findByText(/12 charges approved in the last 30 days, all under £40\.00\./)).toBeTruthy()
    const link = screen.getByRole('link', { name: 'Approve charges under £50.00 automatically' })
    const query = new URLSearchParams((link.getAttribute('href') ?? '').split('?')[1])
    expect(link.getAttribute('href')).toMatch(/^\/settings\/approvals\?/)
    expect(query.get('requires')).toBe('charge')
    expect(query.get('under')).toBe('50')
    // The count alone is not asked for when the amounts said enough.
    expect(counted).not.toHaveBeenCalled()
  })

  it('offers nothing below three', async () => {
    amounts.mockResolvedValue(approvedInPounds(2))
    counted.mockResolvedValue({ requires: 'charge', approved: 2, may_delegate: true })
    show(['approvals:answer', 'approvals:delegate'])
    await waitFor(() => expect(counted).toHaveBeenCalled())
    expect(screen.queryByRole('link')).toBeNull()
  })

  it('is never shown on a follow-up', async () => {
    amounts.mockResolvedValue(approvedInPounds(12))
    show(['approvals:answer', 'approvals:delegate'], { followup: 'unsent' })
    // Nothing is asked for, so nothing can arrive to be shown.
    expect(amounts).not.toHaveBeenCalled()
    expect(counted).not.toHaveBeenCalled()
    expect(screen.queryByRole('link')).toBeNull()
  })
})

describe('ApprovalPrompt and auto-approval', () => {
  beforeEach(() => {
    counted.mockReset()
    amounts.mockReset()
    // An act that moves no money: nothing approved carried an amount.
    amounts.mockResolvedValue({ requires: 'charge', days: 30, currencies: [], may_delegate: true })
  })

  it('links to the rules for the act once it has been approved three times', async () => {
    counted.mockResolvedValue({ requires: 'charge', approved: 3, may_delegate: true })
    show(['approvals:answer', 'approvals:delegate'])
    const link = await screen.findByRole('link', { name: 'Rules for “charge”' })
    expect(link.getAttribute('href')).toBe('/settings/approvals?requires=charge')
    // A link, and never a box ticked on the way past.
    expect(screen.queryByRole('checkbox')).toBeNull()
  })

  it('offers nothing below three', async () => {
    counted.mockResolvedValue({ requires: 'charge', approved: 2, may_delegate: true })
    show(['approvals:answer', 'approvals:delegate'])
    await waitFor(() => expect(counted).toHaveBeenCalled())
    expect(screen.queryByRole('link')).toBeNull()
  })

  it('does not ask, or offer, for somebody who could not set a policy', () => {
    show(['approvals:answer'])
    expect(counted).not.toHaveBeenCalled()
    expect(screen.queryByRole('link')).toBeNull()
  })
})
