import { ruleLine, ruleSource, type Kind, type RuleOperation } from '../lib/approvalRules'

const CHIP: Record<string, string> = {
  ask: 'border-surface-300 text-surface-600 dark:border-surface-700 dark:text-surface-400',
  on: 'border-green-300 text-green-800 dark:border-green-900 dark:text-green-300',
  trial: 'border-amber-300 text-amber-800 dark:border-amber-900 dark:text-amber-300',
  never: 'border-red-300 text-red-700 dark:border-red-900 dark:text-red-300',
}

/** What each line of an operation's rule says, one per rule in force. */
function lines(op: RuleOperation): { kind: Kind; text: string; note: string | null }[] {
  if (op.fixed.never) return [{ kind: 'never', text: ruleLine(op, undefined), note: null }]
  if (op.rules.length === 0) return [{ kind: 'ask', text: ruleLine(op, undefined), note: null }]
  const everyone = op.rules.length === 1 && op.rules[0].agents.length === op.agents.length
  const out = op.rules.map((r) => {
    const who = everyone ? null : `${r.agents.map((a) => a.name).join(', ')} only`
    return {
      kind: r.state as Kind,
      text: `${r.state === 'trial' ? 'Trial · ' : ''}${ruleLine(op, r)}`,
      note: [who, ruleSource(r)].filter(Boolean).join(', ') || null,
    }
  })
  // The agents no rule speaks for still ask.
  const covered = new Set(op.rules.flatMap((r) => r.agents.map((a) => a.id)))
  const asking = op.agents.filter((a) => !covered.has(a.id))
  if (asking.length > 0) {
    out.push({ kind: 'ask', text: 'Always asks', note: asking.map((a) => a.name).join(', ') })
  }
  return out
}

/** An operation's rule in words, as the table and a skill's page show it. */
export default function RuleChips({ op }: { op: RuleOperation }) {
  return (
    <>
      {lines(op).map((l, i) => (
        <div key={i} className={i > 0 ? 'mt-1.5' : ''}>
          <span className={`inline-block whitespace-nowrap rounded-full border px-2 py-0.5 text-xs ${CHIP[l.kind]}`}>
            {l.text}
          </span>
          {l.note && <div className="mt-0.5 text-xs text-surface-600 dark:text-surface-400">{l.note}</div>}
        </div>
      ))}
    </>
  )
}
