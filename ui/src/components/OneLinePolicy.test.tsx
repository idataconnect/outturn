import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import OneLinePolicy from './OneLinePolicy'
import { ApiError } from '../lib/api'
import {
  countOf,
  createPolicy,
  kindOf,
  oneLineLink,
  putBands,
  roundUnder,
  type BandOperation,
  type BandsPage,
} from '../lib/autoApproval'

vi.mock('../lib/api', async (actual) => ({
  ...(await actual<typeof import('../lib/api')>()),
  allPages: vi.fn(async () => [{ id: 'a1', name: 'Front desk' }]),
}))
vi.mock('../lib/autoApproval', async (actual) => ({
  ...(await actual<typeof import('../lib/autoApproval')>()),
  putBands: vi.fn(async () => ({})),
  createPolicy: vi.fn(async () => ({ id: 'p-new' })),
}))

const put = vi.mocked(putBands)
const create = vi.mocked(createPolicy)

const gbp = (amount: number) => ({ amount, currency: 'GBP', minor_units: 2, display: `£${(amount / 100).toFixed(2)}` })

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

const bands: BandsPage = {
  operations: [charge],
  currencies: [{ currency: 'GBP', minor_units: 2, band: null, effective: null }],
  unused: [],
  may_write: true,
}

const show = (at = '/settings/auto-approval') =>
  render(
    <MemoryRouter initialEntries={[at]}>
      <Routes>
        <Route path="/settings/auto-approval" element={<OneLinePolicy bands={bands} onChanged={vi.fn()} />} />
        <Route path="/settings/auto-approval/:id" element={<p>the policy page</p>} />
      </Routes>
    </MemoryRouter>,
  )

describe('OneLinePolicy', () => {
  beforeEach(() => {
    put.mockClear()
    create.mockClear()
  })

  it('reads as the sentence it sets, starting from the band in force', () => {
    show()
    expect(screen.getByText('Charges')).toBeTruthy()
    expect(screen.getByLabelText('Under')).toHaveProperty('value', '50')
    expect(screen.getByText('go through automatically, up to')).toBeTruthy()
  })

  it('writes only the policy when the band is already that', async () => {
    show()
    await userEvent.type(screen.getByLabelText('A day'), '500')
    await userEvent.click(screen.getByRole('button', { name: 'Create policy' }))
    expect(put).not.toHaveBeenCalled()
    expect(create).toHaveBeenCalledWith(
      expect.objectContaining({
        selector: 'act',
        requires: 'charge',
        max_risk: 'low',
        daily_totals: { GBP: 50000 },
        agent_id: null,
        mode: 'trial',
      }),
    )
    expect(await screen.findByText('the policy page')).toBeTruthy()
  })

  it('is prefilled from an offer, and moves the band first', async () => {
    show(oneLineLink('charge', 'GBP', 4000))
    expect(screen.getByLabelText('Under')).toHaveProperty('value', '40')
    await userEvent.selectOptions(await screen.findByLabelText('Agent'), 'a1')
    await userEvent.type(screen.getByLabelText('A day'), '250.50')
    await userEvent.click(screen.getByRole('button', { name: 'Create policy' }))
    expect(put).toHaveBeenCalledWith({
      skill_id: 'sk1',
      requires: 'charge',
      matches: 'POST /charges',
      low: 4000,
      medium: 20000,
    })
    expect(create).toHaveBeenCalledWith(expect.objectContaining({ agent_id: 'a1', daily_totals: { GBP: 25050 } }))
  })

  it('stops at a band the floor refuses, before any policy exists', async () => {
    put.mockRejectedValueOnce(new ApiError(400, 'low reaches past the floor'))
    show(oneLineLink('charge', 'GBP', 900000))
    await userEvent.type(screen.getByLabelText('A day'), '10000')
    await userEvent.click(screen.getByRole('button', { name: 'Create policy' }))
    expect((await screen.findByRole('alert')).textContent).toMatch(/floor/)
    expect(create).not.toHaveBeenCalled()
  })

  it('refuses a figure that is not an amount', async () => {
    show()
    await userEvent.type(screen.getByLabelText('A day'), 'lots')
    await userEvent.click(screen.getByRole('button', { name: 'Create policy' }))
    expect(screen.getByRole('alert').textContent).toMatch(/daily total/)
    expect(create).not.toHaveBeenCalled()
  })
})

describe('the words for an offer', () => {
  it('counts and names a kind', () => {
    expect(countOf(12, 'charge')).toBe('12 charges')
    expect(countOf(1, 'charge')).toBe('1 charge')
    expect(countOf(3, 'send_email')).toBe('3 approvals of “send email”')
    expect(kindOf('charge')).toBe('Charges')
  })

  it('finds a round figure every amount was under', () => {
    expect(roundUnder(3999, 2)).toBe(4000)
    expect(roundUnder(4000, 2)).toBe(5000)
    expect(roundUnder(12, 2)).toBe(100)
    expect(roundUnder(123456, 2)).toBe(200000)
  })
})
