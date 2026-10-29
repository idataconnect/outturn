import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import OneLinePolicy from './OneLinePolicy'
import { ApiError } from '../lib/api'
import {
  countOf,
  createOneLine,
  kindOf,
  oneLineLink,
  type BandOperation,
  type BandsPage,
} from '../lib/autoApproval'

vi.mock('../lib/api', async (actual) => ({
  ...(await actual<typeof import('../lib/api')>()),
  allPages: vi.fn(async () => [{ id: 'a1', name: 'Front desk' }]),
}))
vi.mock('../lib/autoApproval', async (actual) => ({
  ...(await actual<typeof import('../lib/autoApproval')>()),
  createOneLine: vi.fn(async () => ({ id: 'p-new' })),
}))

const create = vi.mocked(createOneLine)

const gbp = (amount: number) => ({
  amount,
  currency: 'GBP',
  minor_units: 2,
  display: `£${(amount / 100).toFixed(2)}`,
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
    create.mockClear()
  })

  it('reads as the sentence it sets, starting from the band in force', () => {
    show()
    expect(screen.getByText('Charges')).toBeTruthy()
    expect(screen.getByLabelText('Under')).toHaveProperty('value', '50')
    expect(screen.getByText('go through automatically, up to')).toBeTruthy()
  })

  it('names the skill the currency comes from, and the operation it changes', () => {
    show()
    expect(screen.getByText('in Bookings')).toBeTruthy()
    expect(screen.getByText(/Sets the low band of POST \/charges in Bookings, which counts in GBP/)).toBeTruthy()
  })

  it('sends the bands and the policy as one write', async () => {
    show()
    await userEvent.type(screen.getByLabelText('A day'), '500')
    await userEvent.click(screen.getByRole('button', { name: 'Create policy' }))
    expect(create).toHaveBeenCalledTimes(1)
    expect(create).toHaveBeenCalledWith(
      expect.objectContaining({
        requires: 'charge',
        currency: 'GBP',
        low: 5000,
        daily_total: 50000,
        agent_id: null,
        mode: 'trial',
      }),
    )
    expect(await screen.findByText('the policy page')).toBeTruthy()
  })

  it('is prefilled from an offer', async () => {
    show(oneLineLink('charge', 'GBP', gbp(4000)))
    expect(screen.getByLabelText('Under')).toHaveProperty('value', '40')
    await userEvent.selectOptions(await screen.findByLabelText('Agent'), 'a1')
    await userEvent.type(screen.getByLabelText('A day'), '250.50')
    await userEvent.click(screen.getByRole('button', { name: 'Create policy' }))
    expect(create).toHaveBeenCalledWith(
      expect.objectContaining({ agent_id: 'a1', low: 4000, daily_total: 25050 }),
    )
  })

  it('says why nothing was set when the API refuses it', async () => {
    create.mockRejectedValueOnce(new ApiError(400, 'low reaches past the floor'))
    show(oneLineLink('charge', 'GBP', gbp(900000)))
    await userEvent.type(screen.getByLabelText('A day'), '10000')
    await userEvent.click(screen.getByRole('button', { name: 'Create policy' }))
    expect((await screen.findByRole('alert')).textContent).toMatch(/floor/)
    expect(screen.queryByText('the policy page')).toBeNull()
  })

  it('starts from a band in whole pounds as pounds', () => {
    const pounds = { ...charge, minor_units: 0, effective: { low: { ...gbp(30), minor_units: 0, display: '£30', figure: '30' }, medium: null } }
    render(
      <MemoryRouter initialEntries={['/settings/auto-approval']}>
        <OneLinePolicy bands={{ ...bands, operations: [pounds] }} onChanged={vi.fn()} />
      </MemoryRouter>,
    )
    expect(screen.getByLabelText('Under')).toHaveProperty('value', '30')
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
})
