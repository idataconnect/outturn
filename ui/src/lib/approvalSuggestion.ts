import type { Annotation, Risk, SkillSource, SuggestedApproval } from './skills'

type Operation = NonNullable<SkillSource['outline']>['operations'][number]

/** What the operator has settled on for one operation: the wizard's guess,
 *  moved to wherever they put it. */
export type Choice = {
  gate: boolean
  risk: Risk
  /** The amount field, or '' for none. */
  amount: string
  currency: string
  minorUnits: string
}

export function choiceFrom(s: SuggestedApproval): Choice {
  return {
    gate: s.gate,
    risk: s.risk,
    amount: s.amount?.field ?? '',
    currency: s.amount?.currency ?? '',
    minorUnits: s.amount?.minor_units === undefined ? '' : String(s.amount.minor_units),
  }
}

/** The path a base URL puts in front of every operation's, without a trailing slash. */
export function basePath(baseUrl: string | null): string {
  if (!baseUrl) return ''
  try {
    return new URL(baseUrl).pathname.replace(/\/+$/, '')
  } catch {
    return ''
  }
}

/** Why a rule cannot be written for this operation, or null if one can. A rule
 *  is keyed on body fields, and one with none would cover every request. */
export function cannotWrite(s: SuggestedApproval): string | null {
  return s.binds.length === 0
    ? 'It sends no body fields for a rule to be about, so a rule cannot be written.'
    : null
}

/** The `approval` frontmatter for a confirmed choice. Only what the operator
 *  confirmed is written: an amount appears when one is chosen, and the currency
 *  and units only where they were given. */
export function approvalYaml(op: Operation, c: Choice, baseUrl: string | null): string {
  const s = op.suggested_approval!
  const binds = c.amount && !s.binds.includes(c.amount) ? [...s.binds, c.amount] : s.binds
  const numbers = c.amount && !s.numbers.includes(c.amount) ? [...s.numbers, c.amount] : s.numbers
  const lines = [
    'approval:',
    `  requires: ${s.requires}`,
    `  matches: ${op.method.toUpperCase()} ${basePath(baseUrl)}${s.matches_path}`,
    `  binds: [${binds.join(', ')}]`,
    `  risk: ${c.risk}`,
    `  numbers: [${numbers.join(', ')}]`,
  ]
  if (c.amount) {
    const parts = [`field: ${c.amount}`]
    if (c.currency.trim()) parts.push(`currency: ${c.currency.trim().toUpperCase()}`)
    if (c.minorUnits.trim()) parts.push(`minor_units: ${c.minorUnits.trim()}`)
    lines.push(`  amount: { ${parts.join(', ')} }`)
  }
  return lines.join('\n')
}

/** Operations a bulk accept may write: gated by the guess, able to have a rule,
 *  and without one already. */
export function acceptable(
  operations: Operation[],
  annotations: Annotation[],
): Operation[] {
  const ruled = new Set(
    annotations.filter((a) => a.kind === 'approval' && a.level === 'operation').map((a) => a.target),
  )
  return operations.filter(
    (o) =>
      o.suggested_approval?.gate && !cannotWrite(o.suggested_approval) && !ruled.has(o.name),
  )
}
