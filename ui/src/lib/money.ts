/**
 * Amounts as money, and the risk a request works out to from one. See
 * docs/auto-approval.md, *Risk from the amount*.
 */

import type { Risk } from './skills'

/** How risky a request works out to be, from its operation and its amount:
 *  the one type, declared beside the skills that declare base risks. */
export type { Risk }

/** An amount as the API hands it over: the number in minor units, what it is
 *  in, how a person reads it, and the figure a form starts from. A page shows
 *  it with `show`, in the reader's own locale; the API's `display` is what the
 *  model is told, and the fallback for a code the browser does not know. */
export type Money = {
  amount: number
  currency: string
  minor_units: number
  display: string
  /** `50`, `12.50`: the amount in whole units, as it is typed. */
  figure: string
}

/**
 * An amount as the reader writes money: `£1,250.00` in one locale,
 * `1.250,00 £` in another, with the currency's own decimals rather than the
 * locale's. The figure is built from the digits, so nothing is lost to
 * floating point on its way to the formatter.
 */
export function show(
  m: Pick<Money, 'amount' | 'currency' | 'minor_units' | 'display'>,
  locale?: string,
): string {
  try {
    const digits = Math.max(0, Math.min(m.minor_units, 20))
    const abs = Math.abs(m.amount).toString().padStart(digits + 1, '0')
    const whole = abs.slice(0, abs.length - digits)
    const part = abs.slice(abs.length - digits)
    const figure = `${m.amount < 0 ? '-' : ''}${whole}${digits ? `.${part}` : ''}`
    return new Intl.NumberFormat(locale, {
      style: 'currency',
      currency: m.currency,
      minimumFractionDigits: digits,
      maximumFractionDigits: digits,
    }).format(figure as unknown as number)
  } catch {
    return m.display
  }
}

/** The currency's sign as `show` writes it in the reader's locale -- `£`,
 *  `US$` -- for beside a field an amount is typed into; its code where the
 *  browser does not know it. */
export function currencySign(currency: string, locale?: string): string {
  try {
    const part = new Intl.NumberFormat(locale, { style: 'currency', currency })
      .formatToParts(0)
      .find((p) => p.type === 'currency')
    return part?.value ?? currency
  } catch {
    return currency
  }
}

/** Every currency code the browser knows, for a field that asks for one. */
export function currencyCodes(): string[] {
  try {
    return Intl.supportedValuesOf('currency')
  } catch {
    return []
  }
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
