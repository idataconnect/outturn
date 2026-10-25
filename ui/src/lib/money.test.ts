import { describe, expect, it } from 'vitest'

import { moneyOf, riskOf, toMinor } from './money'

describe('money', () => {
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

  it('is exact however long, or refused', () => {
    expect(toMinor('90071992547409.91', 2)).toBe(9007199254740991)
    // One more than a number holds exactly is refused rather than rounded.
    expect(toMinor('90071992547409.92', 2)).toBeNull()
    expect(toMinor('123456789012345678901234567890', 0)).toBeNull()
  })

  it('is read from a payload only when it is whole', () => {
    expect(
      moneyOf({
        money: { amount: 4000, currency: 'GBP', minor_units: 2, display: '£40.00', figure: '40', field: 'amount_pence' },
      }),
    ).toEqual({ amount: 4000, currency: 'GBP', minor_units: 2, display: '£40.00', figure: '40', field: 'amount_pence' })
    expect(moneyOf({ money: { amount: '4000', currency: 'GBP', minor_units: 2, display: '£40.00' } })).toBeNull()
    // Never written here: no display, no money.
    expect(moneyOf({ money: { amount: 4000, currency: 'GBP', minor_units: 2 } })).toBeNull()
    expect(moneyOf({})).toBeNull()
    expect(riskOf({ risk: 'high' })).toBe('high')
    expect(riskOf({ risk: 'severe' })).toBeNull()
  })
})
