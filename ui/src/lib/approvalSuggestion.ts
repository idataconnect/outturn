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
  /** The field a request names its currency in, or '' for none. */
  currencyField: string
}

export function choiceFrom(s: SuggestedApproval): Choice {
  return {
    gate: s.gate,
    risk: s.risk,
    amount: s.amount?.field ?? '',
    currency: s.amount?.currency ?? '',
    minorUnits: s.amount?.minor_units === undefined ? '' : String(s.amount.minor_units),
    currencyField: s.amount?.currency_field ?? '',
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
 *  is keyed on body fields and path parameters, and one with neither would
 *  cover every request. */
export function cannotWrite(s: SuggestedApproval): string | null {
  return s.binds.length === 0
    ? 'It sends no body fields or path parameters for a rule to be about, so a rule cannot be written.'
    : null
}

/** The fields an amount may be: the bound body fields. A path parameter is
 *  not money anybody can be shown. */
export function amountFields(s: SuggestedApproval): string[] {
  const path = s.path_params
  return [...new Set([...s.binds, ...(s.amount ? [s.amount.field] : [])])].filter(
    (f) => !path.includes(f),
  )
}

/** Whether a choice says enough about its amount to be read as money: a
 *  field, the currency, and the digits after the point. The parser refuses an
 *  amount missing either of the last two, so a part-known one is left out of
 *  the rule rather than written and refused -- the operation is still gated,
 *  at its base risk. */
export function amountComplete(c: Choice): boolean {
  return Boolean(c.amount && c.currency.trim() && c.minorUnits.trim())
}

/** The `approval` frontmatter for a confirmed choice. Only what the operator
 *  confirmed is written, and an amount only where it is complete. */
export function approvalYaml(op: Operation, c: Choice, baseUrl: string | null): string {
  const s = op.suggested_approval!
  const currencyField = c.amount ? c.currencyField.trim() : ''
  // Both are read from the body, so both are bound. The path parameters come
  // first in the suggestion's binds already.
  const binds = [...s.binds]
  for (const f of [c.amount, currencyField]) if (f && !binds.includes(f)) binds.push(f)
  const numbers = c.amount && !s.numbers.includes(c.amount) ? [...s.numbers, c.amount] : s.numbers
  const lines = [
    'approval:',
    `  requires: ${s.requires}`,
    `  matches: ${op.method.toUpperCase()} ${basePath(baseUrl)}${s.matches_path}`,
    `  binds: [${binds.join(', ')}]`,
    `  risk: ${c.risk}`,
    `  numbers: [${numbers.join(', ')}]`,
  ]
  if (amountComplete(c)) {
    const parts = [
      `field: ${c.amount}`,
      `currency: ${c.currency.trim().toUpperCase()}`,
      `minor_units: ${c.minorUnits.trim()}`,
    ]
    if (currencyField) parts.push(`currency_field: ${currencyField}`)
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
