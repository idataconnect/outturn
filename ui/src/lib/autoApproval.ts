import { allPages, api } from './api'
import { formatMoney, type Money, type Risk } from './money'

/** One test a policy applies to a field the gate binds. See docs/auto-approval.md. */
export type Op = 'eq' | 'ne' | 'in' | 'not_in' | 'lt' | 'le' | 'gt' | 'ge'
export type Condition = { field: string; op: Op; value: unknown }

/** A policy as the page reads it. */
export type Policy = {
  id: string
  agent_id: string | null
  agent_name: string | null
  effect: 'include' | 'exclude'
  selector: 'risk' | 'act' | 'operation'
  max_risk: string | null
  requires: string | null
  skill_id: string | null
  skill_name: string | null
  matches: string | null
  conditions: Condition[]
  per_turn: number
  per_day: number
  sum_field: string | null
  sum_per_day: number | null
  /** The most of each currency it lets out in a day, in minor units. */
  daily_totals: Record<string, number>
  mode: 'trial' | 'on'
  version: number
  note: string | null
  created_by_name: string | null
  created_at: string
  updated_by_name: string | null
  updated_at: string
  expires_at: string
  revoked_by_name: string | null
  revoked_at: string | null
  auto_approved: number
  state: 'on' | 'trial' | 'revoked' | 'expired' | 'lapsed'
}

/** What a person writes, for a new policy and an edit alike. */
export type WritePolicy = {
  agent_id?: string | null
  effect: 'include' | 'exclude'
  selector: 'risk' | 'act' | 'operation'
  max_risk?: string | null
  requires?: string | null
  skill_id?: string | null
  matches?: string | null
  conditions: Condition[]
  per_turn?: number | null
  per_day?: number | null
  sum_field?: string | null
  sum_per_day?: number | null
  /** The most of each currency it lets out in a day, in minor units. */
  daily_totals?: Record<string, number>
  mode: 'trial' | 'on'
  note?: string | null
  expires_at?: string | null
}

export type Evidence = {
  auto_approved: number
  auto_approved_week: number
  trial_approved: number
  trial_declined: number
  trial_pending: number
  history:
    | {
        item_id: string
        at: string
        requires: string
        binds: { field: string; value: unknown }[]
        answer: 'approved' | 'declined' | 'pending'
        would: boolean
        /** Why it would still have asked, where the amount is what stopped it. */
        why?: string
      }[]
    | null
}

export type Answered = { requires: string; approved: number; may_delegate: boolean }

export const listPolicies = () => allPages<Policy>('/v1/approval-policies')

export const createPolicy = (body: WritePolicy) =>
  api<Policy>('/v1/approval-policies', { method: 'POST', body: JSON.stringify(body) })

export const updatePolicy = (id: string, body: WritePolicy) =>
  api<Policy>(`/v1/approval-policies/${id}`, { method: 'PUT', body: JSON.stringify(body) })

export const revokePolicy = (id: string) =>
  api<Policy>(`/v1/approval-policies/${id}/revoke`, { method: 'POST' })

export const expirePolicy = (id: string) =>
  api<Policy>(`/v1/approval-policies/${id}/expire`, { method: 'POST' })

export const acceptPolicy = (id: string) =>
  api<Policy>(`/v1/approval-policies/${id}/accept`, { method: 'POST' })

export const policyEvidence = (id: string) =>
  api<Evidence>(`/v1/approval-policies/${id}/evidence`)

export const answeredCount = (requires: string) =>
  api<Answered>(`/v1/approvals/answered?requires=${encodeURIComponent(requires)}`)

/** How many yeses to one act before the prompt offers a policy for it. */
export const OFFER_AFTER = 3

/**
 * The link from an approval to a policy for the same act, its fields filled
 * in. The page reads these back: `requires`, `selector`, and a `field` for
 * each bound field, so the conditions start as rows naming them.
 */
export function policyLink(requires: string, fields: string[] = []): string {
  const query = new URLSearchParams({ requires, selector: 'act' })
  for (const field of fields) query.append('field', field)
  return `/settings/auto-approval/new?${query.toString()}`
}

/** A bound on an act or an operation's risk, as the end of its line. */
const upTo = (risk: string | null) =>
  risk === 'low' ? ' of low risk' : risk ? ` of ${risk} risk or lower` : ''

/** What a policy covers, in a line. */
export function covers(
  p: Pick<Policy, 'selector' | 'max_risk' | 'requires' | 'matches' | 'skill_name'>,
): string {
  switch (p.selector) {
    case 'risk':
      return p.max_risk === 'low' ? 'Every low-risk operation' : `Every operation of ${p.max_risk} risk or lower`
    case 'act':
      return `Every “${p.requires}”${upTo(p.max_risk)}`
    case 'operation':
      return `${p.matches}${p.skill_name ? ` in ${p.skill_name}` : ''}${upTo(p.max_risk)}`
  }
}

/** What state a policy is in, as the list says it. */
export const stateLabel: Record<Policy['state'], string> = {
  on: 'On',
  trial: 'Trial',
  revoked: 'Revoked',
  expired: 'Expired',
  lapsed: 'Lapsed: whoever set it has left',
}

/**
 * A typed value as the API reads it. A whole number is a number and true or
 * false a boolean; anything else, or anything in quotes, is text. A list is
 * comma-separated.
 */
export function parseValue(op: Op, raw: string): unknown {
  const one = (s: string): unknown => {
    const t = s.trim()
    const quoted = t.match(/^"(.*)"$/)
    if (quoted) return quoted[1]
    if (/^-?\d+$/.test(t)) return Number(t)
    if (t === 'true' || t === 'false') return t === 'true'
    return t
  }
  if (op === 'in' || op === 'not_in') return raw.split(',').map(one)
  return one(raw)
}

/** Each level's upper bound, as money: below `low` is low, below `medium`
 *  medium. */
export type BandsView = { low: Money | null; medium: Money | null }
/** From each, that level whatever a band says. */
export type FloorView = { medium: Money | null; high: Money | null }

/** A row a workspace set, for a currency or for one operation. */
export type BandRow = {
  id: string
  currency: string
  skill_id: string | null
  requires: string | null
  matches: string | null
  low: number | null
  medium: number | null
  updated_by_name: string | null
  updated_at: string
}

/** One money operation the workspace's agents reach. */
export type BandOperation = {
  skill_id: string
  skill_name: string | null
  requires: string
  matches: string
  field: string
  currency: string
  minor_units: number
  /** What an amount above every band, or one nobody can read, works out to. */
  risk: Risk
  defaults: BandsView
  floor: FloorView
  effective: BandsView
  source: 'operation' | 'currency' | 'operator'
  band_id: string | null
  held_at_floor: boolean
}

export type BandCurrency = {
  currency: string
  minor_units: number
  band: BandRow | null
  effective: BandsView | null
}

export type BandsPage = {
  operations: BandOperation[]
  currencies: BandCurrency[]
  unused: BandRow[]
  may_write: boolean
}

/** A write: a currency, or one operation named as a policy names it, and the
 *  bounds in minor units. */
export type WriteBands = {
  currency?: string
  skill_id?: string
  requires?: string
  matches?: string
  low?: number | null
  medium?: number | null
}

export const listBands = () => api<BandsPage>('/v1/approval-bands')

export const putBands = (body: WriteBands) =>
  api<BandRow>('/v1/approval-bands', { method: 'PUT', body: JSON.stringify(body) })

export const revertBands = (id: string) => api<void>(`/v1/approval-bands/${id}`, { method: 'DELETE' })

export type RoleRef = { role_id: string; name: string }
export type Routes = { high: RoleRef[]; answerers: RoleRef[]; may_write: boolean }

export const getRoutes = () => api<Routes>('/v1/approval-routes')

export const putHighRoute = (roleIds: string[]) =>
  api<Routes>('/v1/approval-routes/high', { method: 'PUT', body: JSON.stringify({ role_ids: roleIds }) })

/** What a workspace approved of one act in one currency. */
export type ApprovedIn = {
  currency: string
  minor_units: number
  approved: number
  max: Money
  total: Money
  suggested_low: Money
}

export type Amounts = { requires: string; days: number; currencies: ApprovedIn[]; may_delegate: boolean }

export const approvedAmounts = (requires: string, days = 30) =>
  api<Amounts>(`/v1/approvals/amounts?requires=${encodeURIComponent(requires)}&days=${days}`)

/**
 * The link to the one-line policy on the Auto-approval page, its kind and,
 * where known, currency and amount filled in.
 */
export function oneLineLink(requires: string, currency?: string, low?: number): string {
  const query = new URLSearchParams({ line: requires })
  if (currency) query.set('currency', currency)
  if (low !== undefined) query.set('low', String(low))
  return `/settings/auto-approval?${query.toString()}`
}

/**
 * Requests of one kind, counted: `12 charges`. A kind of more than one word
 * reads `12 approvals of “send email”`, rather than a guess at its plural.
 */
export function countOf(n: number, requires: string): string {
  const word = requires.replace(/_/g, ' ')
  if (/\s/.test(word)) return `${n} approval${n === 1 ? '' : 's'} of “${word}”`
  return `${n} ${word}${n === 1 ? '' : 's'}`
}

/** Requests of one kind, as the start of a sentence: `Charges`. */
export function kindOf(requires: string): string {
  const word = requires.replace(/_/g, ' ')
  if (/\s/.test(word)) return `Requests for “${word}”`
  return `${word.charAt(0).toUpperCase()}${word.slice(1)}s`
}

/**
 * A round figure every approved amount was under: one more than the largest,
 * up to its leading digit -- £40.00 after a largest of £39.99. Never less than
 * one whole unit.
 */
export function roundUnder(max: number, minorUnits: number): number {
  const above = Math.max(max + 1, 10 ** minorUnits)
  const magnitude = 10 ** (String(above).length - 1)
  return Math.ceil(above / magnitude) * magnitude
}

/** The decimals of each currency the workspace's money operations are in. */
export function decimalsOf(page: BandsPage | null | undefined):Record<string, number> {
  const out: Record<string, number> = {}
  for (const c of page?.currencies ?? []) out[c.currency] = c.minor_units
  for (const o of page?.operations ?? []) out[o.currency] ??= o.minor_units
  return out
}

/** A policy's daily totals as money, with each currency's decimals where they
 *  are known, and the figure in its minor units where they are not. */
export function dailyTotals(totals: Record<string, number> | undefined, decimals: Record<string, number>): string[] {
  return Object.entries(totals ?? {}).map(([currency, amount]) =>
    decimals[currency] === undefined
      ? `${currency} ${amount} in minor units`
      : formatMoney(amount, currency, decimals[currency]),
  )
}
