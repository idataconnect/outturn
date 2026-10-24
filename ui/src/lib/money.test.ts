import { describe, expect, it } from 'vitest'

import { formatMoney, moneyOf, riskOf, toMajor, toMinor } from './money'

describe('money', () => {
  it('is written as the API writes it', () => {
    expect(formatMoney(5000, 'GBP', 2)).toBe('£50.00')
    expect(formatMoney(123456789, 'USD', 2)).toBe('$1,234,567.89')
    expect(formatMoney(1250, 'CHF', 2)).toBe('CHF 12.50')
    expect(formatMoney(500, 'JPY', 0)).toBe('¥500')
    expect(formatMoney(-5, 'EUR', 2)).toBe('-€0.05')
  })

  it('is typed in whole units and kept in minor ones, on the digits', () => {
    expect(toMinor('50', 2)).toBe(5000)
    expect(toMinor('19.99', 2)).toBe(1999)
    expect(toMinor('1,000.5', 2)).toBe(100050)
    expect(toMinor('500', 0)).toBe(500)
    expect(toMinor('1.234', 2)).toBeNull()
    expect(toMinor('-5', 2)).toBeNull()
    expect(toMinor('fifty', 2)).toBeNull()
    expect(toMinor('', 2)).toBeNull()
  })

  it('is shown again to be typed as it would be typed', () => {
    expect(toMajor(5000, 2)).toBe('50')
    expect(toMajor(1250, 2)).toBe('12.50')
    expect(toMajor(500, 0)).toBe('500')
  })

  it('is read from a payload only when it is whole', () => {
    expect(
      moneyOf({ money: { amount: 4000, currency: 'GBP', minor_units: 2, display: '£40.00', field: 'amount_pence' } }),
    ).toEqual({ amount: 4000, currency: 'GBP', minor_units: 2, display: '£40.00', field: 'amount_pence' })
    expect(moneyOf({ money: { amount: '4000', currency: 'GBP', minor_units: 2 } })).toBeNull()
    expect(moneyOf({})).toBeNull()
    expect(riskOf({ risk: 'high' })).toBe('high')
    expect(riskOf({ risk: 'severe' })).toBeNull()
  })
})
