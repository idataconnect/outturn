import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { ShieldCheck } from 'lucide-react'

import ApprovalRulePanel from './ApprovalRulePanel'
import RuleChips from './RuleChips'
import { listRules, sameOperation, type RuleOperation, type RulesPage } from '../lib/approvalRules'
import { paths } from '../lib/paths'
import { useHasAuthority } from '../lib/session'

/**
 * A skill's operations that need approval, each with the rule this workspace
 * has on it, and the same panel Settings → Approvals opens to change one.
 * Declaring the gate stays in the skill; the rule set on it is here beside it.
 *
 * Shown to whoever may read rules, and only where an agent here reaches one
 * of the skill's gated operations: one no agent reaches has no rule to say.
 */
export default function SkillApprovals({ skillId, reloads }: { skillId: string; reloads: number }) {
  const answers = useHasAuthority('approvals:answer')
  const delegates = useHasAuthority('approvals:delegate')
  const readable = answers || delegates
  const [page, setPage] = useState<RulesPage | null>(null)
  const [open, setOpen] = useState<RuleOperation | null>(null)

  useEffect(() => {
    if (!readable) return
    let stale = false
    listRules()
      .then((p) => !stale && setPage(p))
      .catch(() => !stale && setPage(null))
    return () => {
      stale = true
    }
  }, [readable, skillId, reloads])

  const ops = (page?.operations ?? []).filter((o) => o.skill_id === skillId)
  if (!page || ops.length === 0) return null

  return (
    <section className="mt-10" aria-labelledby="skill-approvals">
      <h2
        id="skill-approvals"
        className="flex items-center gap-2 text-lg font-semibold text-surface-900 dark:text-surface-100"
      >
        <ShieldCheck size={18} aria-hidden />
        Approvals
      </h2>
      <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
        The operations here that need a person’s yes, and the rule on each. Every skill’s are together in{' '}
        <Link to={paths.approvals} className="text-brand-700 hover:underline dark:text-brand-300">
          Settings → Approvals
        </Link>
        .
      </p>
      <ul className="mt-4 divide-y divide-surface-200 overflow-hidden rounded-lg border border-surface-200 bg-white dark:divide-surface-800 dark:border-surface-800 dark:bg-surface-900">
        {ops.map((o) => (
          <li key={o.matches + o.requires}>
            <button
              type="button"
              onClick={() => setOpen(open && sameOperation(open, o) ? null : o)}
              aria-expanded={Boolean(open && sameOperation(open, o))}
              className="flex w-full flex-wrap items-start justify-between gap-x-4 gap-y-2 p-4 text-left hover:bg-surface-50 dark:hover:bg-surface-800/60"
            >
              <span className="min-w-0">
                <span className="block text-sm font-medium text-surface-900 dark:text-surface-100">{o.name}</span>
                <span className="block font-mono text-xs text-surface-600 dark:text-surface-400">{o.matches}</span>
              </span>
              <span className="text-right">
                <RuleChips op={o} />
              </span>
            </button>
            {open && sameOperation(open, o) && (
              <div className="border-t border-surface-200 p-4 dark:border-surface-800">
                <ApprovalRulePanel
                  ops={[o]}
                  days={page.days}
                  mayWrite={answers && delegates && page.may_write}
                  onSaved={(next) => {
                    setPage(next)
                    setOpen(null)
                  }}
                  onClose={() => setOpen(null)}
                />
              </div>
            )}
          </li>
        ))}
      </ul>
    </section>
  )
}
