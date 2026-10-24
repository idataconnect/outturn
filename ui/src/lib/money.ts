/**
 * Amounts as money, and the risk a request works out to from one. See
 * docs/auto-approval.md, *Risk from the amount*.
 */

/** How risky a request works out to be, from its operation and its amount. */
export type Risk = 'low' | 'medium' | 'high'

/** An amount as the API hands it over: the number in minor units, what it is
 *  in, and how a person reads it. */
export type Money = { amount: number; currency: string; minor_units: number; display: string }

/** The symbols written in place of a code, the same ones the API writes. */
const symbols: Record<string, string> = {
  GBP: '£',
  USD: '$',
  EUR: '€',
  JPY: '¥',
  INR: '₹',
  AUD: 'A$',
  CAD: 'CA$',
  NZD: 'NZ$',
}

/**
 * An amount in minor units as a person reads it, written as the API writes its
 * `display` -- `£1,250.00`, `CHF 12.50` -- so a figure worked out here reads
 * the same as one handed over.
 */
export function formatMoney(amount: number, currency: string, minorUnits: number): string {
  const scale = 10 ** minorUnits
  const sign = amount < 0 ? '-' : ''
  const abs = Math.abs(Math.trunc(amount))
  const whole = Math.floor(abs / scale)
    .toString()
    .replace(/\B(?=(\d{3})+(?!\d))/g, ',')
  const number = minorUnits === 0 ? whole : `${whole}.${String(abs % scale).padStart(minorUnits, '0')}`
  const symbol = symbols[currency]
  return symbol ? `${sign}${symbol}${number}` : `${sign}${currency} ${number}`
}

/** The symbol or code an amount field is labeled with. */
export const currencyMark = (currency: string) => symbols[currency] ?? currency

/**
 * An amount typed in whole units -- `50`, `12.5`, `1,000` -- in minor units,
 * or null for anything that is not one: a negative, more decimals than the
 * currency has, or text. Worked on the digits rather than by multiplying, so
 * `19.99` is 1999 and not 1998.9999.
 */
export function toMinor(typed: string, minorUnits: number): number | null {
  const t = typed.trim().replace(/,/g, '')
  const m = t.match(/^(\d+)(?:\.(\d*))?$/)
  if (!m) return null
  const part = m[2] ?? ''
  if (part.length > minorUnits) return null
  return Number(m[1] + part.padEnd(minorUnits, '0'))
}

/** An amount in minor units as a form shows it to be typed again: `50`,
 *  `12.50`. */
export function toMajor(amount: number, minorUnits: number): string {
  if (minorUnits === 0) return String(amount)
  const scale = 10 ** minorUnits
  const part = amount % scale
  const whole = Math.floor(amount / scale)
  return part === 0 ? String(whole) : `${whole}.${String(part).padStart(minorUnits, '0')}`
}

/** A queue item's risk, read defensively from its free-form payload. */
export function riskOf(payload: Record<string, unknown>): Risk | null {
  const r = payload.risk
  return r === 'low' || r === 'medium' || r === 'high' ? r : null
}

/** An amount and the bound field it was read from. */
export type FieldMoney = Money & { field?: string }

/** A queue item's amount, where the request carried one the API could trust. */
export function moneyOf(payload: Record<string, unknown>): FieldMoney | null {
  const m = payload.money
  if (!m || typeof m !== 'object') return null
  const { amount, currency, minor_units, display, field } = m as Record<string, unknown>
  if (typeof amount !== 'number' || typeof currency !== 'string' || typeof minor_units !== 'number') {
    return null
  }
  return {
    amount,
    currency,
    minor_units,
    display: typeof display === 'string' ? display : formatMoney(amount, currency, minor_units),
    field: typeof field === 'string' ? field : undefined,
  }
}
