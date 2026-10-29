import { describe, expect, it } from 'vitest'

import { currencySign, moneyOf, riskOf, show, toMinor } from './money'

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

describe('show', () => {
  const money = (amount: number, currency: string, minor_units: number) => ({
    amount,
    currency,
    minor_units,
    display: `${currency} ${amount}`,
  })

  it('writes money as the reader’s locale does, not as one country does', () => {
    expect(show(money(125000, 'USD', 2), 'en-US')).toBe('$1,250.00')
    expect(show(money(125000, 'EUR', 2), 'de-DE')).toBe('1.250,00 €')
  })

  it('keeps the currency’s own decimals', () => {
    expect(show(money(5000, 'JPY', 0), 'en-US')).toBe('¥5,000')
    expect(show(money(12345, 'KWD', 3), 'en-US')).toBe('KWD 12.345')
  })

  it('loses nothing to floating point on a long figure', () => {
    expect(show(money(900719925474099, 'USD', 2), 'en-US')).toBe('$9,007,199,254,740.99')
  })

  it('falls back to what the API wrote for a code the browser does not know', () => {
    expect(show(money(100, 'XYZW', 2), 'en-US')).toBe('XYZW 100')
  })
})

describe('currencySign', () => {
  it('is the sign show writes the amount with, or the code it does not know', () => {
    expect(currencySign('GBP', 'en-US')).toBe('£')
    expect(show({ amount: 100, currency: 'GBP', minor_units: 2, display: '' }, 'en-US').startsWith('£')).toBe(true)
    expect(currencySign('XYZW', 'en-US')).toBe('XYZW')
  })
})
