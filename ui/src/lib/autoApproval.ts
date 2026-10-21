import { allPages, api } from './api'

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

/** What a policy covers, in a line. */
export function covers(
  p: Pick<Policy, 'selector' | 'max_risk' | 'requires' | 'matches' | 'skill_name'>,
): string {
  switch (p.selector) {
    case 'risk':
      return p.max_risk === 'low' ? 'Every low-risk operation' : `Every operation of ${p.max_risk} risk or lower`
    case 'act':
      return `Every “${p.requires}”`
    case 'operation':
      return `${p.matches}${p.skill_name ? ` in ${p.skill_name}` : ''}`
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
