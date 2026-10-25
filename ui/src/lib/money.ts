/**
 * Amounts as money, and the risk a request works out to from one. See
 * docs/auto-approval.md, *Risk from the amount*.
 */

import type { Risk } from './skills'

/** How risky a request works out to be, from its operation and its amount:
 *  the one type, declared beside the skills that declare base risks. */
export type { Risk }

/** An amount as the API hands it over: the number in minor units, what it is
 *  in, how a person reads it, and the figure a form starts from. Every amount
 *  a page shows is the API's `display`; nothing here writes money itself. */
export type Money = {
  amount: number
  currency: string
  minor_units: number
  display: string
  /** `50`, `12.50`: the amount in whole units, as it is typed. */
  figure: string
}

/**
 * An amount typed in whole units -- `50`, `12.5`, `1,000` -- in minor units,
 * or null for anything that is not one: a negative, more decimals than the
 * currency has, text, or a figure too large to be sent exactly. Worked on the
 * digits rather than by multiplying, so `19.99` is 1999 and not 1998.9999, and
 * counted as a BigInt, so a long figure is refused rather than rounded.
 */
export function toMinor(typed: string, minorUnits: number): number | null {
  const t = typed.trim().replace(/,/g, '')
  const m = t.match(/^(\d+)(?:\.(\d*))?$/)
  if (!m) return null
  const part = m[2] ?? ''
  if (part.length > minorUnits) return null
  const minor = BigInt(m[1] + part.padEnd(minorUnits, '0'))
  return minor <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(minor) : null
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
  const { amount, currency, minor_units, display, figure, field } = m as Record<string, unknown>
  if (
    typeof amount !== 'number' ||
    typeof currency !== 'string' ||
    typeof minor_units !== 'number' ||
    typeof display !== 'string'
  ) {
    return null
  }
  return {
    amount,
    currency,
    minor_units,
    display,
    figure: typeof figure === 'string' ? figure : '',
    field: typeof field === 'string' ? field : undefined,
  }
}
