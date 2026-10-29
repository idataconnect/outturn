import { useEffect, useMemo, useState } from 'react'
import { createPortal } from 'react-dom'
import { Link, useSearchParams } from 'react-router'

import ApprovalRulePanel, { type Prefill } from '../components/ApprovalRulePanel'
import HighRiskRoute from '../components/HighRiskRoute'
import { PageHeader } from '../components/IndexPage'
import { ApiError } from '../lib/api'
import {
  listRules,
  ruleLine,
  ruleSource,
  sameOperation,
  type OperationRef,
  type RuleOperation,
  type RulesPage,
} from '../lib/approvalRules'
import { covers, listPolicies, stateLabel, type Policy } from '../lib/autoApproval'
import { paths } from '../lib/paths'
import { useSession } from '../lib/session'
import { useBreakpoint } from '../lib/useBreakpoint'

const control =
  'rounded-md border border-surface-300 bg-white px-2 py-1.5 text-sm text-surface-900 dark:border-surface-700 dark:bg-surface-800 dark:text-surface-100'

const CHIP: Record<string, string> = {
  ask: 'border-surface-300 text-surface-600 dark:border-surface-700 dark:text-surface-400',
  on: 'border-green-300 text-green-800 dark:border-green-900 dark:text-green-300',
  trial: 'border-amber-300 text-amber-800 dark:border-amber-900 dark:text-amber-300',
  never: 'border-red-300 text-red-700 dark:border-red-900 dark:text-red-300',
}

type Kind = keyof typeof CHIP

const key = (o: OperationRef) => `${o.skill_id} ${o.requires} ${o.matches}`

/** What each line of an operation's rule says, one per rule in force. */
function lines(op: RuleOperation): { kind: Kind; text: string; note: string | null }[] {
  if (op.fixed.never) return [{ kind: 'never', text: ruleLine(op, undefined), note: null }]
  if (op.rules.length === 0) return [{ kind: 'ask', text: ruleLine(op, undefined), note: null }]
  const everyone = op.rules.length === 1 && op.rules[0].agents.length === op.agents.length
  const lines = op.rules.map((r) => {
    const who = everyone ? null : `${r.agents.map((a) => a.name).join(', ')} only`
    const from = ruleSource(r)
    return {
      kind: r.state as Kind,
      text: `${r.state === 'trial' ? 'Trial · ' : ''}${ruleLine(op, r)}`,
      note: [who, from].filter(Boolean).join(', ') || null,
    }
  })
  // The agents no rule speaks for still ask.
  const covered = new Set(op.rules.flatMap((r) => r.agents.map((a) => a.id)))
  const asking = op.agents.filter((a) => !covered.has(a.id))
  if (asking.length > 0) {
    lines.push({ kind: 'ask', text: 'Always asks', note: `${asking.map((a) => a.name).join(', ')}` })
  }
  return lines
}

function kindOf(op: RuleOperation): Kind {
  if (op.fixed.never) return 'never'
  if (op.rules.length === 0) return 'ask'
  return op.rules.some((r) => r.state === 'on') ? 'on' : 'trial'
}

const KIND_LABEL: Record<Kind, string> = {
  ask: 'Always asks',
  on: 'Automatic',
  trial: 'In trial',
  never: 'Never automatic',
}

/**
 * Every operation an agent here can reach that needs a person's yes, and the
 * rule on each: one table, and one panel to change a rule in. Broad rules,
 * and who is asked about high risk, follow beneath.
 */
export default function Approvals() {
  const state = useSession()
  const workspaceId = state.status === 'authenticated' ? state.session.workspace_id : null
  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const mayDelegate = authorities.includes('approvals:delegate') && authorities.includes('approvals:answer')
  const [params, setParams] = useSearchParams()
  const beside = useBreakpoint() === 'desktop'

  const [page, setPage] = useState<RulesPage | null>(null)
  const [broad, setBroad] = useState<Policy[]>([])
  const [error, setError] = useState<string | null>(null)
  const [search, setSearch] = useState('')
  const [skill, setSkill] = useState('')
  const [agent, setAgent] = useState('')
  const [kind, setKind] = useState<'' | Kind>('')
  const [selected, setSelected] = useState<string[]>([])
  const [open, setOpen] = useState<string[]>([])
  const [prefill, setPrefill] = useState<Prefill | undefined>()

  useEffect(() => {
    let stale = false
    void Promise.all([listRules(), listPolicies().catch(() => [])])
      .then(([rules, policies]) => {
        if (stale) return
        setPage(rules)
        setBroad(policies.filter((p) => p.selector === 'risk' && p.state !== 'revoked'))
      })
      .catch((e) => !stale && setError(e instanceof ApiError ? e.message : 'The rules could not be loaded.'))
    return () => {
      stale = true
    }
  }, [workspaceId])

  // Arriving from a question in the queue or a skill: that operation's panel,
  // with the amount it was offered.
  useEffect(() => {
    if (!page) return
    const asked = {
      skill_id: params.get('skill_id') ?? '',
      requires: params.get('requires') ?? '',
      matches: params.get('matches') ?? '',
    }
    if (!asked.skill_id) return
    const op = page.operations.find((o) => sameOperation(o, asked))
    if (op) {
      setOpen([key(op)])
      setPrefill(params.get('under') ? { under: params.get('under') ?? undefined } : undefined)
    }
    setParams({}, { replace: true })
  }, [page, params, setParams])

  const ops = useMemo(() => page?.operations ?? [], [page])
  const skills = [...new Set(ops.map((o) => o.skill_name))].sort()
  const agents = [...new Map(ops.flatMap((o) => o.agents).map((a) => [a.id, a])).values()].sort((a, b) =>
    a.name.localeCompare(b.name),
  )

  const shown = useMemo(() => {
    const q = search.trim().toLowerCase()
    return ops.filter(
      (o) =>
        (!q || `${o.name} ${o.matches} ${o.skill_name}`.toLowerCase().includes(q)) &&
        (!skill || o.skill_name === skill) &&
        (!agent || o.agents.some((a) => a.id === agent)) &&
        (!kind || kindOf(o) === kind),
    )
  }, [ops, search, skill, agent, kind])

  const groups = useMemo(() => {
    const by = new Map<string, RuleOperation[]>()
    for (const o of shown) by.set(o.skill_name, [...(by.get(o.skill_name) ?? []), o])
    return [...by.entries()]
  }, [shown])

  const openOps = ops.filter((o) => open.includes(key(o)))
  const chosen = ops.filter((o) => selected.includes(key(o)))
  // Several share a rule only where its amounts mean the same thing.
  const currencies = new Set(chosen.map((o) => o.amount?.currency ?? ''))
  const bulkable = chosen.length > 1 && currencies.size === 1
  const writable = mayDelegate && page?.may_write

  function toggle(k: string) {
    setSelected((s) => (s.includes(k) ? s.filter((x) => x !== k) : [...s, k]))
  }

  function saved(next: RulesPage) {
    setPage(next)
    setOpen([])
    setSelected([])
    setPrefill(undefined)
  }

  const panel = openOps.length > 0 && page && (
    <ApprovalRulePanel
      key={open.join('|')}
      ops={openOps}
      days={page.days}
      mayWrite={Boolean(writable)}
      prefill={prefill}
      onSaved={saved}
      onClose={() => {
        setOpen([])
        setPrefill(undefined)
      }}
    />
  )

  // With the panel beside it, the table gives up the agents column, which the
  // panel names.
  const narrow = Boolean(panel) && beside

  return (
    <div className="p-6">
      <div className="max-w-3xl">
        <PageHeader
          title="Approvals"
          description="Every operation your agents can reach that needs a person’s yes, and when it doesn’t."
        />
      </div>

      {error && (
        <p role="alert" className="mt-4 text-sm text-red-600 dark:text-red-400">
          {error}
        </p>
      )}

      <div className={`mt-6 grid gap-6 ${panel && beside ? 'grid-cols-[minmax(0,1fr)_380px] items-start' : ''}`}>
        <section aria-label="Operations" className="min-w-0">
          {ops.length > 0 && (
            <div className="mb-3 flex flex-wrap gap-2">
              <input
                type="search"
                aria-label="Search operations"
                placeholder="Search operations"
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                className={`${control} min-w-40 flex-1`}
              />
              {skills.length > 1 && (
                <select aria-label="Skill" className={control} value={skill} onChange={(e) => setSkill(e.target.value)}>
                  <option value="">All skills</option>
                  {skills.map((s) => (
                    <option key={s}>{s}</option>
                  ))}
                </select>
              )}
              {agents.length > 1 && (
                <select aria-label="Agent" className={control} value={agent} onChange={(e) => setAgent(e.target.value)}>
                  <option value="">All agents</option>
                  {agents.map((a) => (
                    <option key={a.id} value={a.id}>
                      {a.name}
                    </option>
                  ))}
                </select>
              )}
              <select
                aria-label="Rule"
                className={control}
                value={kind}
                onChange={(e) => setKind(e.target.value as '' | Kind)}
              >
                <option value="">Any rule</option>
                {(Object.keys(KIND_LABEL) as Kind[]).map((k) => (
                  <option key={k} value={k}>
                    {KIND_LABEL[k]}
                  </option>
                ))}
              </select>
            </div>
          )}

          {writable && chosen.length > 1 && (
            <div className="mb-3 flex flex-wrap items-center gap-3 rounded-md border border-brand-300 bg-brand-50 px-3 py-2 text-sm dark:border-brand-800 dark:bg-brand-950/40">
              <span className="text-surface-900 dark:text-surface-100">{chosen.length} selected</span>
              {bulkable ? (
                <button
                  type="button"
                  onClick={() => {
                    setPrefill(undefined)
                    setOpen(chosen.map(key))
                  }}
                  className="rounded-md bg-brand-700 px-3 py-1 text-white hover:bg-brand-600 dark:bg-brand-600"
                >
                  Set a rule for these
                </button>
              ) : (
                <span className="text-surface-600 dark:text-surface-400">
                  These count in different currencies, so they can’t share a rule.
                </span>
              )}
            </div>
          )}

          <div className="overflow-x-auto rounded-lg border border-surface-200 bg-white dark:border-surface-800 dark:bg-surface-900">
            {!page ? (
              <p className="p-4 text-sm text-surface-600 dark:text-surface-400">{error ? '' : 'Loading…'}</p>
            ) : ops.length === 0 ? (
              <p className="p-4 text-sm text-surface-600 dark:text-surface-400">
                No agent here reaches an operation that needs approval.
              </p>
            ) : shown.length === 0 ? (
              <p className="p-4 text-sm text-surface-600 dark:text-surface-400">Nothing matches.</p>
            ) : (
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b border-surface-200 text-left text-xs text-surface-600 dark:border-surface-800 dark:text-surface-400">
                    {writable && <th className="w-8 px-3 py-2"><span className="sr-only">Select</span></th>}
                    <th className="px-3 py-2 font-medium">Operation</th>
                    {!narrow && <th className="hidden px-3 py-2 font-medium md:table-cell">Agents</th>}
                    <th className="px-3 py-2 font-medium">Rule</th>
                    <th className="hidden whitespace-nowrap px-3 py-2 font-medium sm:table-cell">Last {page.days} days</th>
                  </tr>
                </thead>
                {groups.map(([name, rows]) => (
                  <tbody key={name}>
                    <tr className="bg-surface-50 dark:bg-surface-800/50">
                      <th
                        colSpan={(writable ? 5 : 4) - (narrow ? 1 : 0)}
                        scope="rowgroup"
                        className="px-3 py-1.5 text-left text-xs font-semibold text-surface-700 dark:text-surface-300"
                      >
                        {name}
                      </th>
                    </tr>
                    {rows.map((o) => {
                      const k = key(o)
                      return (
                        <tr
                          key={k}
                          onClick={() => {
                            setPrefill(undefined)
                            setOpen([k])
                          }}
                          aria-selected={open.length === 1 && open[0] === k}
                          className="cursor-pointer border-t border-surface-200 align-top hover:bg-surface-50 aria-selected:bg-brand-50 dark:border-surface-800 dark:hover:bg-surface-800/60 dark:aria-selected:bg-brand-950/40"
                        >
                          {writable && (
                            <td className="px-3 py-3" onClick={(e) => e.stopPropagation()}>
                              <input
                                type="checkbox"
                                aria-label={`Select ${o.name}`}
                                checked={selected.includes(k)}
                                onChange={() => toggle(k)}
                                className="accent-brand-600"
                              />
                            </td>
                          )}
                          <td className="px-3 py-3">
                            <button
                              type="button"
                              className="text-left font-medium text-surface-900 hover:underline dark:text-surface-100"
                            >
                              {o.name}
                            </button>
                            <div className="whitespace-nowrap font-mono text-xs text-surface-600 dark:text-surface-400">{o.matches}</div>
                          </td>
                          {!narrow && (
                            <td className="hidden px-3 py-3 text-xs text-surface-600 md:table-cell dark:text-surface-400">
                              {o.agents.map((a) => a.name).join(', ')}
                            </td>
                          )}
                          <td className="px-3 py-3">
                            {lines(o).map((l, i) => (
                              <div key={i} className={i > 0 ? 'mt-1.5' : ''}>
                                <span className={`inline-block whitespace-nowrap rounded-full border px-2 py-0.5 text-xs ${CHIP[l.kind]}`}>
                                  {l.text}
                                </span>
                                {l.note && <div className="mt-0.5 text-xs text-surface-600 dark:text-surface-400">{l.note}</div>}
                              </div>
                            ))}
                          </td>
                          <td className="hidden px-3 py-3 text-xs text-surface-600 sm:table-cell dark:text-surface-400">
                            {o.asked === 0 ? '—' : `${o.asked} asked · ${o.approved} approved`}
                          </td>
                        </tr>
                      )
                    })}
                  </tbody>
                ))}
              </table>
            )}
          </div>

          <section aria-labelledby="broad" className="mt-8">
            <div className="flex items-baseline justify-between gap-4">
              <h2 id="broad" className="text-base font-semibold text-surface-900 dark:text-surface-100">
                Broad rules
              </h2>
              {writable && (
                <Link to={paths.newAutoApproval} className="text-sm text-brand-700 hover:underline dark:text-brand-300">
                  New broad rule
                </Link>
              )}
            </div>
            <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
              A rule by risk, across every skill. An operation’s own rule overrides it.
            </p>
            {broad.length > 0 ? (
              <ul className="mt-3 divide-y divide-surface-200 rounded-lg border border-surface-200 bg-white text-sm dark:divide-surface-800 dark:border-surface-800 dark:bg-surface-900">
                {broad.map((p) => (
                  <li key={p.id}>
                    <Link
                      to={paths.autoApproval(p.id)}
                      className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 px-3 py-2.5 hover:bg-surface-50 dark:hover:bg-surface-800/60"
                    >
                      <span className="text-surface-900 dark:text-surface-100">
                        {p.effect === 'exclude' ? 'Except ' : ''}
                        {covers(p)}
                        {p.agent_name ? `, ${p.agent_name} only` : ''}
                      </span>
                      <span className="text-xs text-surface-600 dark:text-surface-400">
                        {stateLabel[p.state]} · until {new Date(p.expires_at).toLocaleDateString()}
                      </span>
                    </Link>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="mt-3 text-sm text-surface-600 dark:text-surface-400">None.</p>
            )}
          </section>

          <div className="max-w-3xl">
            <HighRiskRoute />
          </div>
        </section>

        {/* Beside the table where there is room; over it where there is not. */}
        {panel &&
          (beside ? (
            <aside aria-label="Rule" className="sticky top-6">
              {panel}
            </aside>
          ) : (
            // On the body, so the page's own layers cannot leave the navigation
            // undimmed beside it.
            createPortal(
            <div
              className="fixed inset-0 z-40 overflow-y-auto bg-black/50 p-4"
              role="dialog"
              aria-modal="true"
              aria-label="Rule"
              onKeyDown={(e) => e.key === 'Escape' && setOpen([])}
            >
              <div className="mx-auto max-w-md">{panel}</div>
            </div>,
            document.body,
            )
          ))}
      </div>
    </div>
  )
}
