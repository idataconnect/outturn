import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import ApprovalRulePanel from './ApprovalRulePanel'
import { ApiError } from '../lib/api'
import { previewRule, putRule, type RuleOperation } from '../lib/approvalRules'

vi.mock('../lib/approvalRules', async (actual) => ({
  ...(await actual<typeof import('../lib/approvalRules')>()),
  putRule: vi.fn(async () => ({ operations: [], days: 30, may_write: true })),
  previewRule: vi.fn(async () => ({
    days: 30,
    asked: 14,
    would: 11,
    would_total: { amount: 28640, currency: 'GBP', minor_units: 2, display: '£286.40', figure: '286.40' },
    would_declined: 1,
    history: [],
  })),
}))

const put = vi.mocked(putRule)
const preview = vi.mocked(previewRule)

const gbp = (amount: number) => ({
  amount,
  currency: 'GBP',
  minor_units: 2,
  display: `£${(amount / 100).toFixed(2)}`,
  figure: amount % 100 ? (amount / 100).toFixed(2) : String(amount / 100),
})

const charge: RuleOperation = {
  skill_id: 'sk1',
  skill_name: 'Bookings',
  requires: 'charge',
  matches: 'POST /charges',
  name: 'Charge payment account',
  agents: [
    { id: 'a1', name: 'Front desk' },
    { id: 'a2', name: 'Concierge' },
  ],
  amount: { field: 'amount_pence', currency: 'GBP', minor_units: 2, standard_minor_units: 2 },
  fixed: { never: false, beyond_deployment: false, most: gbp(500000) },
  rules: [],
  asked: 14,
  approved: 13,
  declined: 1,
}

const panel = (ops: RuleOperation[], extra: Partial<Parameters<typeof ApprovalRulePanel>[0]> = {}) =>
  render(
    <ApprovalRulePanel ops={ops} days={30} mayWrite onSaved={vi.fn()} onClose={vi.fn()} {...extra} />,
  )

describe('ApprovalRulePanel', () => {
  beforeEach(() => {
    put.mockClear()
    preview.mockClear()
  })

  it('says what the operation is, where it comes from and who reaches it', () => {
    panel([charge])
    expect(screen.getByRole('heading', { name: 'Charge payment account' })).toBeTruthy()
    expect(screen.getByText('POST /charges')).toBeTruthy()
    expect(screen.getByText('Reached by Front desk, Concierge')).toBeTruthy()
    expect(screen.getByRole('radio', { name: 'Always asks' }).getAttribute('aria-checked')).toBe('true')
  })

  it('writes an automatic rule in the currency’s standard units', async () => {
    panel([charge])
    await userEvent.click(screen.getByRole('radio', { name: 'Automatic' }))
    await userEvent.type(screen.getByLabelText('Only under'), '50')
    await userEvent.type(screen.getByLabelText('At most a day'), '500')
    await userEvent.click(screen.getByRole('button', { name: 'Save rule' }))
    expect(put).toHaveBeenCalledWith(
      expect.objectContaining({
        operations: [{ skill_id: 'sk1', requires: 'charge', matches: 'POST /charges' }],
        automatic: true,
        under: 5000,
        daily_total: 50000,
        mode: 'trial',
        agent_id: null,
      }),
    )
  })

  it('shows what the rule would have done, declines first', async () => {
    panel([charge])
    await userEvent.click(screen.getByRole('radio', { name: 'Automatic' }))
    await userEvent.type(screen.getByLabelText('Only under'), '50')
    expect(await screen.findByText(/Would have let 11 of 14 through/)).toBeTruthy()
    expect(screen.getByText('1 of those was declined by a person.')).toBeTruthy()
    expect(preview).toHaveBeenCalledWith(expect.objectContaining({ matches: 'POST /charges' }), expect.objectContaining({ under: 5000 }))
  })

  it('refuses an amount that is not one before asking the API', async () => {
    panel([charge])
    await userEvent.click(screen.getByRole('radio', { name: 'Automatic' }))
    await userEvent.type(screen.getByLabelText('Only under'), 'fifty')
    await userEvent.click(screen.getByRole('button', { name: 'Save rule' }))
    expect(put).not.toHaveBeenCalled()
    expect(screen.getByRole('alert').textContent).toMatch(/figure in GBP/)
  })

  it('says why the API refused', async () => {
    put.mockRejectedValueOnce(new ApiError(400, 'POST /charges: at most £200.00 can go through without a person'))
    panel([charge])
    await userEvent.click(screen.getByRole('radio', { name: 'Automatic' }))
    await userEvent.type(screen.getByLabelText('Only under'), '300')
    await userEvent.click(screen.getByRole('button', { name: 'Save rule' }))
    expect((await screen.findByRole('alert')).textContent).toMatch(/at most £200.00/)
  })

  it('locks an operation the skill says always asks', () => {
    panel([{ ...charge, fixed: { ...charge.fixed, never: true } }])
    expect(screen.getByText(/says a person is always asked/)).toBeTruthy()
    expect(screen.queryByRole('button', { name: 'Save rule' })).toBeNull()
  })

  it('starts from the rule in force, and from an amount it was offered', () => {
    const own = {
      policy_id: 'p1',
      selector: 'operation' as const,
      requires: 'charge',
      max_risk: null,
      state: 'on' as const,
      agents: charge.agents,
      under: gbp(3000),
      per_turn: 5,
      per_day: 50,
      daily_total: gbp(20000),
      expires_at: '2027-01-27T23:59:59Z',
      set_by: 'Dana',
    }
    const { unmount } = panel([{ ...charge, rules: [own] }])
    expect(screen.getByLabelText('Only under')).toHaveProperty('value', '30')
    expect(screen.getByLabelText('At most a day')).toHaveProperty('value', '200')
    unmount()
    panel([charge], { prefill: { under: '45' } })
    expect(screen.getByLabelText('Only under')).toHaveProperty('value', '45')
  })

  it('sets one rule for several operations, totalled each', async () => {
    const refund = { ...charge, matches: 'POST /refunds', name: 'Refund' }
    panel([charge, refund])
    expect(screen.getByRole('heading', { name: '2 operations' })).toBeTruthy()
    await userEvent.click(screen.getByRole('radio', { name: 'Automatic' }))
    expect(screen.getByText('For each operation')).toBeTruthy()
    await userEvent.type(screen.getByLabelText('Only under'), '20')
    await userEvent.click(screen.getByRole('button', { name: 'Save rule' }))
    expect(put.mock.calls[0][0].operations).toHaveLength(2)
  })
})

describe('ApprovalRulePanel preview', () => {
  it('says why there is no preview when the API refuses one', async () => {
    vi.mocked(previewRule).mockRejectedValueOnce(
      new ApiError(400, 'POST /charges: at most £200.00 can go through without a person'),
    )
    render(<ApprovalRulePanel ops={[charge]} days={30} mayWrite onSaved={vi.fn()} onClose={vi.fn()} />)
    await userEvent.click(screen.getByRole('radio', { name: 'Automatic' }))
    await userEvent.type(screen.getByLabelText('Only under'), '300')
    expect(await screen.findByText(/at most £200.00/)).toBeTruthy()
  })
})
