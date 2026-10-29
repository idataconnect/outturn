/**
 * Approval rules: every gated operation an agent can reach, and the one rule
 * in force on it. See docs/approval-rules.md.
 */

import { api } from './api'
import { show, type Money } from './money'

export type AgentRef = { id: string; name: string }

export type OperationRef = { skill_id: string; requires: string; matches: string }

export type InForce = {
  policy_id: string
  /** `operation` when the rule is the operation's own. */
  selector: 'risk' | 'act' | 'operation'
  requires: string | null
  max_risk: string | null
  state: 'on' | 'trial'
  agents: AgentRef[]
  under: Money | null
  per_turn: number
  per_day: number
  daily_total: Money | null
  expires_at: string
  set_by: string | null
}

export type RuleOperation = OperationRef & {
  skill_name: string
  name: string
  agents: AgentRef[]
  amount: { field: string; currency: string; minor_units: number; standard_minor_units: number } | null
  fixed: { never: boolean; beyond_deployment: boolean; most: Money | null }
  rules: InForce[]
  asked: number
  approved: number
  declined: number
}

export type RulesPage = { operations: RuleOperation[]; days: number; may_write: boolean }

/** What a person sets. Amounts in the currency's standard units. */
export type WriteRule = {
  operations: OperationRef[]
  agent_id?: string | null
  automatic: boolean
  under?: number | null
  daily_total?: number | null
  per_turn?: number | null
  per_day?: number | null
  mode: 'trial' | 'on'
  expires_at?: string | null
}

export type Past = {
  item_id: string
  at: string
  answer: 'approved' | 'declined' | 'pending'
  would: boolean
  why?: string
}

export type Preview = {
  days: number
  asked: number
  would: number
  would_total: Money | null
  would_declined: number
  history: Past[]
}

export const listRules = () => api<RulesPage>('/v1/approval-rules')

export const putRule = (rule: WriteRule) =>
  api<RulesPage>('/v1/approval-rules', { method: 'PUT', body: JSON.stringify(rule) })

export function previewRule(
  op: OperationRef,
  rule: { agent_id?: string | null; under?: number | null; daily_total?: number | null },
): Promise<Preview> {
  const q = new URLSearchParams({ skill_id: op.skill_id, requires: op.requires, matches: op.matches })
  if (rule.agent_id) q.set('agent_id', rule.agent_id)
  if (rule.under != null) q.set('under', String(rule.under))
  if (rule.daily_total != null) q.set('daily_total', String(rule.daily_total))
  return api<Preview>(`/v1/approval-rules/preview?${q}`)
}

/** The same operation, however its reference was spelled. */
export const sameOperation = (a: OperationRef, b: OperationRef) =>
  a.skill_id === b.skill_id && a.requires === b.requires && a.matches === b.matches

/** A rule as its chip says it. */
export function ruleLine(op: RuleOperation, rule: InForce | undefined): string {
  if (op.fixed.never) return 'Never automatic'
  if (!rule) return 'Always asks'
  const parts = [rule.under ? `Automatic under ${show(rule.under)}` : 'Automatic']
  if (rule.daily_total) parts.push(`${show(rule.daily_total)} a day`)
  return parts.join(' · ')
}

/** Where a rule that is not the operation's own comes from. */
export function ruleSource(rule: InForce): string | null {
  if (rule.selector === 'operation') return null
  if (rule.selector === 'act') return `by the rule for every “${rule.requires}”`
  return `by the broad rule for ${rule.max_risk ?? 'low'} risk`
}

/**
 * The link from a question in the queue to the rules for the same act: its
 * operation's panel where only one operation needs it, the table narrowed to
 * them where several do, with the amount to start from as its figure.
 */
export function ruleLink(requires: string, under?: Money): string {
  const query = new URLSearchParams({ requires })
  if (under) query.set('under', under.figure)
  return `/settings/approvals?${query.toString()}`
}

export type Kind = 'ask' | 'on' | 'trial' | 'never'

export const KIND_LABEL: Record<Kind, string> = {
  ask: 'Always asks',
  on: 'Automatic',
  trial: 'In trial',
  never: 'Never automatic',
}

/** What an operation's rule is, taken as a whole: for filtering by it. */
export function kindOf(op: RuleOperation): Kind {
  if (op.fixed.never) return 'never'
  if (op.rules.length === 0) return 'ask'
  return op.rules.some((r) => r.state === 'on') ? 'on' : 'trial'
}
