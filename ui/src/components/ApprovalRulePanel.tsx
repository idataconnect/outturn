import { useEffect, useId, useState } from 'react'
import { Lock, X } from 'lucide-react'

import { ApiError } from '../lib/api'
import {
  previewRule,
  putRule,
  ruleSource,
  type AgentRef,
  type Preview,
  type RuleOperation,
  type RulesPage,
} from '../lib/approvalRules'
import { iconButton } from '../lib/buttons'
import { show, toMinor } from '../lib/money'

const field =
  'mt-1 block w-full rounded-md border border-surface-300 bg-white px-3 py-2 text-sm text-surface-900 focus:outline-none focus:ring-2 focus:ring-brand-500/40 dark:border-surface-700 dark:bg-surface-900 dark:text-surface-100'
const label = 'block text-xs font-medium text-surface-700 dark:text-surface-300'
const hint = 'mt-1 text-xs text-surface-600 dark:text-surface-400'

/** The counts a rule carries unless somebody changes them; the API's own. */
const PER_TURN = 5
const PER_DAY = 50
/** How far out a new rule ends, as the API has it. */
const LIFETIME_DAYS = 90

/** What the panel starts from when it opens on a question in the queue. */
export type Prefill = { under?: string }

/** The currency's sign in the reader's locale, `£`, or its code. */
function sign(currency: string): string {
  try {
    const part = new Intl.NumberFormat(undefined, { style: 'currency', currency, currencyDisplay: 'narrowSymbol' })
      .formatToParts(0)
      .find((p) => p.type === 'currency')
    return part?.value ?? currency
  } catch {
    return currency
  }
}

/** A date input's value, `2027-01-27`, for a moment. */
const day = (d: Date) => {
  const local = new Date(d.getTime() - d.getTimezoneOffset() * 60_000)
  return local.toISOString().slice(0, 10)
}

/** The agents every operation here is reached by, which a rule may name one of. */
function sharedAgents(ops: RuleOperation[]): AgentRef[] {
  const [first, ...rest] = ops
  return (first?.agents ?? []).filter((a) => rest.every((o) => o.agents.some((x) => x.id === a.id)))
}

function MoneyInput({
  currency,
  value,
  onChange,
  labelledBy,
}: {
  currency: string
  value: string
  onChange: (v: string) => void
  labelledBy: string
}) {
  return (
    <div className="mt-1 flex items-center rounded-md border border-surface-300 bg-white focus-within:ring-2 focus-within:ring-brand-500/40 dark:border-surface-700 dark:bg-surface-900">
      <span className="pl-3 text-sm text-surface-500" aria-hidden>
        {sign(currency)}
      </span>
      <input
        aria-labelledby={labelledBy}
        inputMode="decimal"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="w-full rounded-md bg-transparent px-2 py-2 text-sm text-surface-900 focus:outline-none dark:text-surface-100"
      />
    </div>
  )
}

/**
 * One rule, on one operation or several, for every agent that reaches them or
 * one of them: the same panel whether it opens from the table, a skill, or a
 * question in the queue.
 */
export default function ApprovalRulePanel({
  ops,
  days,
  mayWrite,
  prefill,
  onSaved,
  onClose,
}: {
  ops: RuleOperation[]
  days: number
  mayWrite: boolean
  prefill?: Prefill
  onSaved: (page: RulesPage) => void
  onClose: () => void
}) {
  const one = ops.length === 1 ? ops[0] : null
  const money = ops[0]?.amount ?? null
  const agents = sharedAgents(ops)
  const never = ops.some((o) => o.fixed.never)
  const beyond = ops.some((o) => o.fixed.beyond_deployment)
  const own = one?.rules.find((r) => r.selector === 'operation')
  // What speaks for it now where it has no rule of its own, which saving one
  // overrides.
  const broader = one?.rules.filter((r) => r.selector !== 'operation') ?? []
  // The lowest limit any of them is held to, which is what a shared rule is.
  const most = ops
    .map((o) => o.fixed.most)
    .filter((m) => m !== null)
    .sort((a, b) => a.amount - b.amount)[0]

  const [automatic, setAutomatic] = useState(() => Boolean(own) || Boolean(prefill?.under))
  const [under, setUnder] = useState(() => prefill?.under ?? own?.under?.figure ?? '')
  const [total, setTotal] = useState(() => own?.daily_total?.figure ?? '')
  const [perDay, setPerDay] = useState(() => String(own?.per_day ?? PER_DAY))
  const [agentId, setAgentId] = useState(() =>
    own && own.agents.length === 1 && one && one.agents.length > 1 ? own.agents[0].id : '',
  )
  const [until, setUntil] = useState(() => {
    if (own) return day(new Date(own.expires_at))
    const d = new Date()
    d.setDate(d.getDate() + LIFETIME_DAYS)
    return day(d)
  })
  const [trial, setTrial] = useState(() => (own ? own.state === 'trial' : true))
  const [preview, setPreview] = useState<Preview | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const digits = money?.standard_minor_units ?? 0
  const underMinor = money ? toMinor(under, digits) : null
  const totalMinor = money && total.trim() ? toMinor(total, digits) : null

  // What it would have done, worked out by the API as the gateway would,
  // once the figures have stopped changing.
  useEffect(() => {
    if (!one || !automatic || (money && !underMinor)) {
      setPreview(null)
      return
    }
    let stale = false
    const timer = setTimeout(() => {
      previewRule(one, { agent_id: agentId || null, under: underMinor, daily_total: totalMinor })
        .then((p) => !stale && setPreview(p))
        .catch(() => !stale && setPreview(null))
    }, 300)
    return () => {
      stale = true
      clearTimeout(timer)
    }
  }, [one, automatic, money, underMinor, totalMinor, agentId])

  const ids = { under: useId(), total: useId(), title: useId() }

  async function save(e: React.FormEvent) {
    e.preventDefault()
    setError(null)
    if (automatic && money && !underMinor) {
      setError(`The amount to go through under is a figure in ${money.currency} above zero.`)
      return
    }
    if (automatic && money && total.trim() && totalMinor === null) {
      setError(`The day's total is a figure in ${money.currency}.`)
      return
    }
    const count = Number(perDay)
    if (automatic && !(Number.isInteger(count) && count > 0)) {
      setError('Requests a day is a whole number above zero.')
      return
    }
    setBusy(true)
    try {
      const page = await putRule({
        operations: ops.map(({ skill_id, requires, matches }) => ({ skill_id, requires, matches })),
        agent_id: agentId || null,
        automatic,
        under: automatic ? underMinor : null,
        daily_total: automatic ? totalMinor : null,
        per_turn: automatic ? Math.min(PER_TURN, count) : null,
        per_day: automatic ? count : null,
        mode: trial ? 'trial' : 'on',
        // The end of the chosen day.
        expires_at: automatic ? new Date(`${until}T23:59:59`).toISOString() : null,
      })
      onSaved(page)
    } catch (err) {
      setError(err instanceof ApiError ? err.message : 'The rule could not be saved.')
    } finally {
      setBusy(false)
    }
  }

  const reachedBy = (one?.agents ?? agents).map((a) => a.name).join(', ')

  return (
    <form
      onSubmit={save}
      aria-labelledby={ids.title}
      className="rounded-lg border border-surface-200 bg-white p-5 dark:border-surface-800 dark:bg-surface-900"
    >
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h2 id={ids.title} className="text-base font-semibold text-surface-900 dark:text-surface-100">
            {one ? one.name : `${ops.length} operations`}
          </h2>
          <p className="mt-1 text-xs text-surface-600 dark:text-surface-400">
            {one ? (
              <>
                {one.skill_name} · <span className="font-mono">{one.matches}</span>
              </>
            ) : (
              ops.map((o) => o.name).join(', ')
            )}
          </p>
          {reachedBy && <p className="mt-1 text-xs text-surface-600 dark:text-surface-400">Reached by {reachedBy}</p>}
        </div>
        <button type="button" aria-label="Close" title="Close" onClick={onClose} className={iconButton}>
          <X size={16} aria-hidden />
        </button>
      </div>

      {broader.length > 0 && (
        <p className="mt-3 text-xs text-surface-600 dark:text-surface-400">
          {broader
            .map(
              (r) =>
                `${r.agents.map((a) => a.name).join(', ')} ${r.agents.length === 1 ? 'is' : 'are'} ${
                  r.state === 'trial' ? 'in trial' : 'automatic'
                } ${ruleSource(r)}`,
            )
            .join('; ')}
          . A rule saved here takes its place.
        </p>
      )}

      {never ? (
        <p className="mt-4 flex gap-2 rounded-md border border-dashed border-red-300 p-3 text-sm text-red-700 dark:border-red-900 dark:text-red-300">
          <Lock size={16} className="mt-0.5 shrink-0" aria-hidden />
          {one ? `The ${one.skill_name} skill` : 'A skill here'} says a person is always asked. A workspace can’t change that.
        </p>
      ) : (
        <fieldset disabled={!mayWrite || busy} className="mt-4 space-y-4">
          <legend className="sr-only">Rule</legend>
          <div role="radiogroup" aria-label="When a person is asked" className="grid grid-cols-2 overflow-hidden rounded-md border border-surface-300 text-sm dark:border-surface-700">
            {[
              { value: false, text: 'Always asks' },
              { value: true, text: 'Automatic' },
            ].map((o) => (
              <button
                key={o.text}
                type="button"
                role="radio"
                aria-checked={automatic === o.value}
                disabled={o.value && beyond}
                onClick={() => setAutomatic(o.value)}
                className={`px-3 py-2 disabled:opacity-50 ${
                  automatic === o.value
                    ? 'bg-brand-700 text-white dark:bg-brand-600'
                    : 'text-surface-700 hover:bg-surface-100 dark:text-surface-300 dark:hover:bg-surface-800'
                }`}
              >
                {o.text}
              </button>
            ))}
          </div>
          {beyond && (
            <p className={hint}>This deployment doesn’t let {one ? 'this operation' : 'all of these'} go through without a person.</p>
          )}

          {automatic && (
            <>
              {money && (
                <div>
                  <span id={ids.under} className={label}>
                    Only under
                  </span>
                  <MoneyInput currency={money.currency} value={under} onChange={setUnder} labelledBy={ids.under} />
                  {most && <p className={hint}>At most {show(most)}</p>}
                </div>
              )}
              <div className="grid grid-cols-2 gap-3">
                {money && (
                  <div>
                    <span id={ids.total} className={label}>
                      At most a day
                    </span>
                    <MoneyInput currency={money.currency} value={total} onChange={setTotal} labelledBy={ids.total} />
                    {!one && <p className={hint}>For each operation</p>}
                  </div>
                )}
                <label className={label}>
                  Requests a day
                  <input inputMode="numeric" className={field} value={perDay} onChange={(e) => setPerDay(e.target.value)} />
                </label>
              </div>
              {agents.length > 1 && (
                <label className={label}>
                  Agents
                  <select className={field} value={agentId} onChange={(e) => setAgentId(e.target.value)}>
                    <option value="">Every agent that reaches {one ? 'it' : 'them'}</option>
                    {agents.map((a) => (
                      <option key={a.id} value={a.id}>
                        {a.name}
                      </option>
                    ))}
                  </select>
                </label>
              )}
              <label className={label}>
                Until
                <input type="date" className={field} value={until} onChange={(e) => setUntil(e.target.value)} />
              </label>
              <label className="flex items-start justify-between gap-4 text-sm text-surface-900 dark:text-surface-100">
                <span>
                  Trial
                  <span className={`block ${hint}`}>
                    People are still asked, and each question says whether this rule would have approved it.
                  </span>
                </span>
                <input
                  type="checkbox"
                  role="switch"
                  aria-checked={trial}
                  checked={trial}
                  onChange={(e) => setTrial(e.target.checked)}
                  className="mt-1 h-4 w-4 accent-brand-600"
                />
              </label>
            </>
          )}
        </fieldset>
      )}

      {one && (
        <section aria-label="Evidence" className="mt-5 border-t border-surface-200 pt-4 text-sm dark:border-surface-800">
          <h3 className="font-medium text-surface-900 dark:text-surface-100">Last {days} days</h3>
          {automatic && preview ? (
            <>
              <p className="mt-1 text-surface-700 dark:text-surface-300">
                {preview.asked === 0
                  ? 'Nothing was asked, so there is nothing to test this rule against yet.'
                  : `Would have let ${preview.would} of ${preview.asked} through${
                      preview.would_total && preview.would > 0 ? `, ${show(preview.would_total)} in all` : ''
                    }.`}
              </p>
              {preview.would_declined > 0 && (
                <p className="mt-1 text-amber-700 dark:text-amber-300">
                  {preview.would_declined === 1
                    ? '1 of those was declined by a person.'
                    : `${preview.would_declined} of those were declined by a person.`}
                </p>
              )}
            </>
          ) : (
            <p className="mt-1 text-surface-700 dark:text-surface-300">
              {one.asked === 0
                ? 'Not asked.'
                : `Asked ${one.asked} ${one.asked === 1 ? 'time' : 'times'}: ${one.approved} approved, ${one.declined} declined.`}
            </p>
          )}
        </section>
      )}

      {error && (
        <p role="alert" className="mt-4 text-sm text-red-600 dark:text-red-400">
          {error}
        </p>
      )}

      {!never && mayWrite && (
        <div className="mt-5 flex gap-2">
          <button
            type="submit"
            disabled={busy}
            className="rounded-md bg-brand-700 px-4 py-2 text-sm font-medium text-white hover:bg-brand-600 disabled:opacity-50 dark:bg-brand-600 dark:hover:bg-brand-500"
          >
            {busy ? 'Saving…' : 'Save rule'}
          </button>
          <button
            type="button"
            onClick={onClose}
            className="rounded-md border border-surface-300 px-4 py-2 text-sm text-surface-700 hover:bg-surface-100 dark:border-surface-700 dark:text-surface-300 dark:hover:bg-surface-800"
          >
            Cancel
          </button>
        </div>
      )}
    </form>
  )
}
