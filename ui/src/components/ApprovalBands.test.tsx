import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import ApprovalBands from './ApprovalBands'
import { ApiError } from '../lib/api'
import { putBands, revertBands, type BandOperation, type BandsPage } from '../lib/autoApproval'

vi.mock('../lib/autoApproval', async (actual) => ({
  ...(await actual<typeof import('../lib/autoApproval')>()),
  putBands: vi.fn(async () => ({})),
  revertBands: vi.fn(async () => undefined),
}))

const put = vi.mocked(putBands)
const revert = vi.mocked(revertBands)

const gbp = (amount: number) => ({
  amount,
  currency: 'GBP',
  minor_units: 2,
  display: `£${(amount / 100).toLocaleString('en-GB', { minimumFractionDigits: 2 })}`,
  figure: amount % 100 ? (amount / 100).toFixed(2) : String(amount / 100),
})

const charge: BandOperation = {
  skill_id: 'sk1',
  skill_name: 'Bookings',
  requires: 'charge',
  matches: 'POST /charges',
  field: 'amount_pence',
  currency: 'GBP',
  minor_units: 2,
  risk: 'high',
  defaults: { low: gbp(5000), medium: gbp(20000) },
  floor: { medium: null, high: gbp(500000) },
  effective: { low: gbp(5000), medium: gbp(20000) },
  source: 'operator',
  band_id: null,
  held_at_floor: false,
}

const page = (over: Partial<BandsPage> = {}): BandsPage => ({
  operations: [charge],
  currencies: [{ currency: 'GBP', minor_units: 2, band: null, effective: null }],
  unused: [],
  may_write: true,
  ...over,
})

describe('ApprovalBands', () => {
  beforeEach(() => {
    put.mockClear()
    revert.mockClear()
  })

  it('says each operation’s bands as money, where they come from, and its floor', () => {
    render(<ApprovalBands page={page()} onChanged={vi.fn()} />)
    expect(screen.getByText('Low below £50.00, medium below £200.00, high risk from there')).toBeTruthy()
    expect(screen.getByText("The skill's defaults")).toBeTruthy()
    expect(screen.getByText(/From £5,000.00 high/)).toBeTruthy()
    expect(screen.getByText('None set: each operation has its own bands, or the skill’s defaults.')).toBeTruthy()
  })

  it('writes an operation’s bands in minor units, typed in whole ones', async () => {
    const onChanged = vi.fn()
    render(<ApprovalBands page={page()} onChanged={onChanged} />)
    await userEvent.click(screen.getByRole('button', { name: 'Edit bands for this operation' }))
    const form = screen.getByRole('form', { name: 'Edit bands for this operation' })
    const low = within(form).getByLabelText(/^Low below/)
    expect(low).toHaveProperty('value', '50')
    await userEvent.clear(low)
    await userEvent.type(low, '75.50')
    await userEvent.click(within(form).getByRole('button', { name: 'Save' }))
    expect(put).toHaveBeenCalledWith({
      skill_id: 'sk1',
      requires: 'charge',
      matches: 'POST /charges',
      low: 7550,
      medium: 20000,
    })
    expect(onChanged).toHaveBeenCalled()
  })

  it('writes a currency’s bands by the currency alone', async () => {
    render(<ApprovalBands page={page()} onChanged={vi.fn()} />)
    await userEvent.click(screen.getByRole('button', { name: 'Edit bands for every GBP operation' }))
    await userEvent.type(screen.getByLabelText(/^Low below/), '20')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    expect(put).toHaveBeenCalledWith({ currency: 'GBP', low: 2000, medium: null })
  })

  it('shows the reason a band past the floor is refused', async () => {
    put.mockRejectedValueOnce(new ApiError(400, 'low reaches past the floor of £5,000.00'))
    render(<ApprovalBands page={page()} onChanged={vi.fn()} />)
    await userEvent.click(screen.getByRole('button', { name: 'Edit bands for this operation' }))
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    expect((await screen.findByRole('alert')).textContent).toMatch(/past the floor/)
  })

  it('reverts a row the workspace set, and says which one wins', async () => {
    render(
      <ApprovalBands
        page={page({ operations: [{ ...charge, source: 'operation', band_id: 'b1', effective: { low: gbp(2000), medium: null } }] })}
        onChanged={vi.fn()}
      />,
    )
    expect(screen.getByText('Set for this operation', { exact: false })).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Revert bands for this operation' }))
    expect(revert).toHaveBeenCalledWith('b1')
  })

  it('says plainly when a currency’s bands are held at an operation’s floor', () => {
    render(
      <ApprovalBands page={page({ operations: [{ ...charge, source: 'currency', held_at_floor: true }] })} onChanged={vi.fn()} />,
    )
    expect(screen.getByText(/reach past this operation's floor/)).toBeTruthy()
  })

  it('offers no change to somebody who may not write', () => {
    render(<ApprovalBands page={page({ may_write: false })} onChanged={vi.fn()} />)
    expect(screen.queryByRole('button')).toBeNull()
    expect(screen.getByText('Changed only by people who may set policies.')).toBeTruthy()
  })
})
