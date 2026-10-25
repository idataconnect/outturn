import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router'
import { Plus, Trash2 } from 'lucide-react'

import { ApiError, allPages, api } from '../lib/api'
import {
  acceptPolicy,
  createPolicy,
  expirePolicy,
  policyEvidence,
  revokePolicy,
  covers,
  decimalsOf,
  listBands,
  parseValue,
  stateLabel,
  updatePolicy,
  type BandsPage,
  type Condition,
  type Evidence,
  type Op,
  type Policy,
  type WritePolicy,
} from '../lib/autoApproval'
import { iconButtonDanger } from '../lib/buttons'
import { toMinor } from '../lib/money'
import { paths } from '../lib/paths'
import { useSession } from '../lib/session'

const ops: { value: Op; label: string }[] = [
  { value: 'eq', label: 'is' },
  { value: 'ne', label: 'is not' },
  { value: 'in', label: 'is one of' },
  { value: 'not_in', label: 'is not one of' },
  { value: 'le', label: 'at most' },
  { value: 'lt', label: 'below' },
  { value: 'ge', label: 'at least' },
  { value: 'gt', label: 'above' },
]

/** A condition as the form holds it: the value as typed. */
type Row = { field: string; op: Op; value: string }

/** A daily total as the form holds it: the amount in whole units, as typed. */
type Total = { currency: string; amount: string }

/** A value as the form shows it again. */
function showValue(value: unknown): string {
  const one = (v: unknown) =>
    typeof v === 'string' && (/^-?\d+$/.test(v) || v === 'true' || v === 'false') ? `"${v}"` : String(v)
  return Array.isArray(value) ? value.map(one).join(', ') : one(value)
}

const field =
  'mt-1 block w-full rounded-md border border-surface-300 bg-white px-2 py-1.5 text-sm dark:border-surface-700 dark:bg-surface-900'
const label = 'block text-sm font-medium text-surface-700 dark:text-surface-300'
const hint = 'mt-1 text-xs text-surface-500 dark:text-surface-400'
const button =
  'rounded-md border border-surface-300 px-3 py-1.5 text-sm text-surface-700 hover:bg-surface-50 disabled:opacity-50 dark:border-surface-700 dark:text-surface-300 dark:hover:bg-surface-800'

type Named = { id: string; name: string }

/**
 * A new policy, or one already set: what it covers, what it tests, its caps,
 * and the record so far beside it.
 *
 * A new one may be prefilled from the address -- `requires`, `selector`,
 * `max_risk`, `skill_id`, `matches`, `agent_id`, and a `field` for each bound
 * field -- which is how an approval links here.
 */
export default function AutoApprovalEditor() {
  const { id } = useParams()
  const [params] = useSearchParams()
  const navigate = useNavigate()
  const state = useSession()
  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canWrite = authorities.includes('approvals:delegate') && authorities.includes('approvals:answer')

  const [existing, setExisting] = useState<Policy | null>(null)
  const [evidence, setEvidence] = useState<Evidence | null>(null)
  const [agents, setAgents] = useState<Named[]>([])
  const [skills, setSkills] = useState<Named[]>([])
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const [effect, setEffect] = useState<'include' | 'exclude'>('include')
  const [selector, setSelector] = useState<'risk' | 'act' | 'operation'>(() => {
    const s = params.get('selector')
    return s === 'risk' || s === 'operation' ? s : 'act'
  })
  // Required for a risk selector; for an act or an operation, empty is every
  // risk the operator's ceiling lets through.
  const [maxRisk, setMaxRisk] = useState(
    () => params.get('max_risk') ?? (params.get('selector') === 'risk' ? 'low' : ''),
  )
  const [bands, setBands] = useState<BandsPage | null>(null)
  const [totals, setTotals] = useState<Total[]>([])
  const [requires, setRequires] = useState(params.get('requires') ?? '')
  const [skillId, setSkillId] = useState(params.get('skill_id') ?? '')
  const [matches, setMatches] = useState(params.get('matches') ?? '')
  const [agentId, setAgentId] = useState(params.get('agent_id') ?? '')
  const [rows, setRows] = useState<Row[]>(() =>
    params.getAll('field').map((f) => ({ field: f, op: 'eq' as Op, value: '' })),
  )
  const [perTurn, setPerTurn] = useState('1')
  const [perDay, setPerDay] = useState('10')
  const [sumField, setSumField] = useState('')
  const [sumPerDay, setSumPerDay] = useState('')
  const [mode, setMode] = useState<'trial' | 'on'>('trial')
  const [note, setNote] = useState('')
  const [expires, setExpires] = useState(() => {
    const d = new Date()
    d.setDate(d.getDate() + 90)
    return d.toISOString().slice(0, 10)
  })

  useEffect(() => {
    void allPages<Named>('/v1/agents').then(setAgents).catch(() => setAgents([]))
    void allPages<Named>('/v1/skills').then(setSkills).catch(() => setSkills([]))
    if (!id) void listBands().then(setBands).catch(() => setBands(null))
  }, [id])

  const decimals = decimalsOf(bands)

  useEffect(() => {
    if (!id) return
    let stale = false
    void (async () => {
      try {
        // Together, because a daily total is typed back in whole units, which
        // needs its currency's decimals from the bands to be read again.
        const [p, page] = await Promise.all([
          api<Policy>(`/v1/approval-policies/${id}`),
          listBands().catch(() => null),
        ])
        if (stale) return
        setBands(page)
        setExisting(p)
        setEffect(p.effect)
        setSelector(p.selector)
        setMaxRisk(p.max_risk ?? (p.selector === 'risk' ? 'low' : ''))
        setTotals(
          Object.entries(p.daily_totals ?? {}).map(([currency, amount]) => ({
            currency,
            amount: p.daily_totals_money?.[currency]?.figure ?? String(amount),
          })),
        )
        setRequires(p.requires ?? '')
        setSkillId(p.skill_id ?? '')
        setMatches(p.matches ?? '')
        setAgentId(p.agent_id ?? '')
        setRows(p.conditions.map((c) => ({ field: c.field, op: c.op, value: showValue(c.value) })))
        setPerTurn(String(p.per_turn))
        setPerDay(String(p.per_day))
        setSumField(p.sum_field ?? '')
        setSumPerDay(p.sum_per_day === null ? '' : String(p.sum_per_day))
        setMode(p.mode)
        setNote(p.note ?? '')
        setExpires(p.expires_at.slice(0, 10))
        setEvidence(await policyEvidence(id))
      } catch (e) {
        if (!stale) setError(e instanceof ApiError ? e.message : 'failed to load the policy')
      }
    })()
    return () => {
      stale = true
    }
  }, [id])

  const editable = canWrite && (!existing || existing.state !== 'revoked')

  // A test on a field an operation declares its amount is written in that
  // field's own units, so what those are is said beside it.
  function asMoney(row: Row): string | null {
    const op = bands?.operations.find(
      (o) =>
        o.field === row.field.trim() &&
        (selector === 'risk' || o.requires === requires) &&
        (selector !== 'operation' || (o.skill_id === skillId && o.matches === matches.trim())),
    )
    if (!op) return null
    const part = ['', 'tenths', 'hundredths', 'thousandths', 'ten-thousandths'][op.minor_units]
    return part ? `in ${part} of ${op.currency}` : `in whole ${op.currency}`
  }

  async function save(e: React.FormEvent) {
    e.preventDefault()
    setBusy(true)
    setError(null)
    const conditions: Condition[] = rows
      .filter((r) => r.field.trim())
      .map((r) => ({ field: r.field.trim(), op: r.op, value: parseValue(r.op, r.value) }))
    const daily: Record<string, number> = {}
    for (const t of totals) {
      const currency = t.currency.trim().toUpperCase()
      if (!currency) continue
      const amount = toMinor(t.amount, decimals[currency] ?? 0)
      if (amount === null) {
        setError(`the daily total in ${currency} is not an amount`)
        setBusy(false)
        return
      }
      daily[currency] = amount
    }
    const body: WritePolicy = {
      agent_id: agentId || null,
      effect,
      selector,
      max_risk: selector === 'risk' ? maxRisk || 'low' : maxRisk || null,
      daily_totals: effect === 'exclude' ? {} : daily,
      requires: selector === 'risk' ? null : requires,
      skill_id: selector === 'operation' ? skillId || null : null,
      matches: selector === 'operation' ? matches : null,
      conditions: effect === 'exclude' ? [] : conditions,
      per_turn: Number(perTurn) || null,
      per_day: Number(perDay) || null,
      sum_field: sumField.trim() || null,
      sum_per_day: sumField.trim() ? Number(sumPerDay) : null,
      mode,
      note: note || null,
      // The end of the chosen day, so a policy set to run out today does not
      // already have.
      expires_at: new Date(`${expires}T23:59:59`).toISOString(),
    }
    try {
      const saved = existing ? await updatePolicy(existing.id, body) : await createPolicy(body)
      navigate(paths.autoApproval(saved.id))
      if (existing) {
        setExisting(saved)
        setBusy(false)
      }
    } catch (err) {
      setError(err instanceof ApiError ? err.message : 'the policy could not be saved')
      setBusy(false)
    }
  }

  async function act(what: (id: string) => Promise<Policy>) {
    if (!existing) return
    setBusy(true)
    setError(null)
    try {
      setExisting(await what(existing.id))
    } catch (err) {
      setError(err instanceof ApiError ? err.message : 'that could not be done')
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="p-6 max-w-3xl">
      <Link to={paths.autoApprovals} className="text-sm text-brand-700 hover:underline dark:text-brand-300">
        Auto-approval
      </Link>
      <h1 className="mt-2 text-2xl font-semibold text-surface-900 dark:text-surface-100">
        {existing ? covers(existing) : 'New policy'}
      </h1>
      {existing && (
        <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
          {existing.effect === 'exclude' ? 'Exclusion' : stateLabel[existing.state]} · set by{' '}
          {existing.created_by_name ?? 'somebody no longer here'} on{' '}
          {new Date(existing.created_at).toLocaleDateString()}
          {existing.updated_by_name && existing.version > 1
            ? `, last changed by ${existing.updated_by_name}`
            : ''}
          {existing.revoked_at
            ? `, revoked${existing.revoked_by_name ? ` by ${existing.revoked_by_name}` : ''} on ${new Date(existing.revoked_at).toLocaleDateString()}`
            : ''}
        </p>
      )}

      {error && (
        <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      {evidence && existing?.effect === 'include' && (
        <section className="mt-6 rounded-md border border-surface-200 p-4 dark:border-surface-800">
          <h2 className="text-sm font-semibold text-surface-900 dark:text-surface-100">The record so far</h2>
          <p className="mt-1 text-sm text-surface-700 dark:text-surface-300">
            {evidence.auto_approved} approved automatically, {evidence.auto_approved_week} in the last week.
            In trial, people approved {evidence.trial_approved} and declined {evidence.trial_declined} that this
            would have approved; {evidence.trial_pending} are waiting.
          </p>
          {evidence.history && evidence.history.length > 0 && (
            <>
              <p className="mt-3 text-xs text-surface-600 dark:text-surface-400">
                The last {evidence.history.length} approvals of what this covers, against its tests, the risk
                each amount works out to under the bands as they stand, and the day's total. Fields outside the
                tests are not part of this.
              </p>
              <ul className="mt-2 space-y-1 text-xs">
                {evidence.history.map((h) => (
                  <li
                    key={h.item_id}
                    className={
                      h.would && h.answer === 'declined'
                        ? 'text-red-700 dark:text-red-400'
                        : 'text-surface-700 dark:text-surface-300'
                    }
                  >
                    {new Date(h.at).toLocaleDateString()} · {h.answer} ·{' '}
                    {h.would ? 'this would have approved it' : 'this would still have asked'}
                    {h.why ? `: ${h.why}` : ''} ·{' '}
                    <span className="font-mono">
                      {h.binds.map((b) => `${b.field}=${JSON.stringify(b.value)}`).join(' ')}
                    </span>
                  </li>
                ))}
              </ul>
            </>
          )}
        </section>
      )}

      <form onSubmit={save} className="mt-6 space-y-5">
        <fieldset disabled={!editable || busy} className="space-y-5">
          <div>
            <span className={label}>What it does</span>
            <select className={field} value={effect} onChange={(e) => setEffect(e.target.value as 'include' | 'exclude')}>
              <option value="include">Approves what it covers, within its limits</option>
              <option value="exclude">Keeps what it covers asking a person, whatever else applies</option>
            </select>
          </div>

          <div>
            <span className={label}>What it covers</span>
            <select
              aria-label="Selector"
              className={field}
              value={selector}
              onChange={(e) => setSelector(e.target.value as 'risk' | 'act' | 'operation')}
            >
              <option value="act">Every operation asking for one kind of approval</option>
              <option value="operation">One operation in one skill</option>
              <option value="risk">Every operation up to a risk</option>
            </select>
          </div>

          <label className={label}>
            Highest risk
            <select
              className={field}
              value={maxRisk || (selector === 'risk' ? 'low' : '')}
              onChange={(e) => setMaxRisk(e.target.value)}
            >
              {selector !== 'risk' && <option value="">Any the platform allows</option>}
              <option value="low">Low</option>
              <option value="medium">Medium</option>
              <option value="high">High</option>
            </select>
            <span className={hint}>
              Where an operation moves money, a request's risk is worked out from its amount and the
              workspace's bands.
            </span>
          </label>
          {selector !== 'risk' && (
            <label className={label}>
              Approval kind
              <input className={field} value={requires} onChange={(e) => setRequires(e.target.value)} />
              <span className={hint}>The word an operation's approval asks for, such as charge.</span>
            </label>
          )}
          {selector === 'operation' && (
            <>
              <label className={label}>
                Skill
                <select className={field} value={skillId} onChange={(e) => setSkillId(e.target.value)}>
                  <option value="">Choose a skill</option>
                  {skills.map((s) => (
                    <option key={s.id} value={s.id}>
                      {s.name}
                    </option>
                  ))}
                </select>
              </label>
              <label className={label}>
                Request
                <input
                  className={field}
                  value={matches}
                  placeholder="POST /charges"
                  onChange={(e) => setMatches(e.target.value)}
                />
              </label>
            </>
          )}

          <label className={label}>
            Agent
            <select className={field} value={agentId} onChange={(e) => setAgentId(e.target.value)}>
              <option value="">Every agent in this workspace</option>
              {agents.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.name}
                </option>
              ))}
            </select>
          </label>

          {effect === 'include' && (
            <>
              <div>
                <span className={label}>Tests</span>
                <p className={hint}>
                  Every test has to hold. Only fields the approval binds may be tested, and only fields its
                  operation calls numbers may be compared. A compared field is at least zero unless a test
                  says otherwise. Text in quotes is compared as text.
                </p>
                <div className="mt-2 space-y-2">
                  {rows.map((row, i) => (
                    <div key={i} className="flex gap-2">
                      <input
                        aria-label={`Field ${i + 1}`}
                        className={field}
                        value={row.field}
                        onChange={(e) => setRows(rows.map((r, j) => (j === i ? { ...r, field: e.target.value } : r)))}
                      />
                      <select
                        aria-label={`Test ${i + 1}`}
                        className={field}
                        value={row.op}
                        onChange={(e) => setRows(rows.map((r, j) => (j === i ? { ...r, op: e.target.value as Op } : r)))}
                      >
                        {ops.map((o) => (
                          <option key={o.value} value={o.value}>
                            {o.label}
                          </option>
                        ))}
                      </select>
                      <div className="w-full">
                        <input
                          aria-label={`Value ${i + 1}`}
                          className={field}
                          value={row.value}
                          onChange={(e) => setRows(rows.map((r, j) => (j === i ? { ...r, value: e.target.value } : r)))}
                        />
                        {asMoney(row) && <span className={hint}>{asMoney(row)}</span>}
                      </div>
                      <button
                        type="button"
                        aria-label={`Remove test ${i + 1}`}
                        className={iconButtonDanger}
                        onClick={() => setRows(rows.filter((_, j) => j !== i))}
                      >
                        <Trash2 size={14} aria-hidden />
                      </button>
                    </div>
                  ))}
                  <button
                    type="button"
                    className={`${button} inline-flex items-center gap-1`}
                    onClick={() => setRows([...rows, { field: '', op: 'eq', value: '' }])}
                  >
                    <Plus size={14} aria-hidden /> Test
                  </button>
                </div>
              </div>

              <div className="grid grid-cols-2 gap-4">
                <label className={label}>
                  Per turn, at most
                  <input className={field} inputMode="numeric" value={perTurn} onChange={(e) => setPerTurn(e.target.value)} />
                </label>
                <label className={label}>
                  Per day, at most
                  <input className={field} inputMode="numeric" value={perDay} onChange={(e) => setPerDay(e.target.value)} />
                </label>
                <label className={label}>
                  Summed field
                  <input className={field} value={sumField} onChange={(e) => setSumField(e.target.value)} />
                  <span className={hint}>Optional: one number field whose total per day is capped.</span>
                </label>
                <label className={label}>
                  Total per day, at most
                  <input
                    className={field}
                    inputMode="numeric"
                    value={sumPerDay}
                    onChange={(e) => setSumPerDay(e.target.value)}
                  />
                </label>
              </div>

              <div>
                <span className={label}>Daily totals</span>
                <p className={hint}>
                  The most of each currency this lets out in a day, across everything it covers. With any
                  set, a money operation is covered only in a currency named here.
                </p>
                <div className="mt-2 space-y-2">
                  {totals.map((t, i) => {
                    const known = decimals[t.currency.trim().toUpperCase()]
                    const minor = toMinor(t.amount, known ?? 0)
                    return (
                      <div key={i} className="flex items-center gap-2">
                        <input
                          aria-label={`Currency ${i + 1}`}
                          className={`${field} w-24`}
                          list="policy-currencies"
                          value={t.currency}
                          onChange={(e) =>
                            setTotals(totals.map((x, j) => (j === i ? { ...x, currency: e.target.value } : x)))
                          }
                        />
                        <input
                          aria-label={`Daily total ${i + 1}`}
                          className={field}
                          inputMode="decimal"
                          value={t.amount}
                          onChange={(e) =>
                            setTotals(totals.map((x, j) => (j === i ? { ...x, amount: e.target.value } : x)))
                          }
                        />
                        <span className="w-32 shrink-0 text-xs text-surface-600 dark:text-surface-400">
                          {known === undefined ? 'in minor units' : minor === null ? 'not an amount' : `in whole ${t.currency.trim().toUpperCase()}`}
                        </span>
                        <button
                          type="button"
                          aria-label={`Remove daily total ${i + 1}`}
                          className={iconButtonDanger}
                          onClick={() => setTotals(totals.filter((_, j) => j !== i))}
                        >
                          <Trash2 size={14} aria-hidden />
                        </button>
                      </div>
                    )
                  })}
                  <datalist id="policy-currencies">
                    {Object.keys(decimals).map((c) => (
                      <option key={c} value={c}>
                        {c}
                      </option>
                    ))}
                  </datalist>
                  <button
                    type="button"
                    className={`${button} inline-flex items-center gap-1`}
                    onClick={() =>
                      setTotals([...totals, { currency: Object.keys(decimals)[0] ?? '', amount: '' }])
                    }
                  >
                    <Plus size={14} aria-hidden /> Daily total
                  </button>
                </div>
              </div>

              <label className={label}>
                Mode
                <select className={field} value={mode} onChange={(e) => setMode(e.target.value as 'trial' | 'on')}>
                  <option value="trial">Trial: people are still asked</option>
                  <option value="on">On: covered requests go out without asking</option>
                </select>
              </label>
            </>
          )}

          <label className={label}>
            Expires
            <input type="date" className={field} value={expires} onChange={(e) => setExpires(e.target.value)} />
            <span className={hint}>At most a year away. Renewed by changing this.</span>
          </label>

          <label className={label}>
            Note
            <input className={field} value={note} onChange={(e) => setNote(e.target.value)} />
          </label>
        </fieldset>

        {editable && (
          <div className="flex flex-wrap gap-2">
            <button
              type="submit"
              disabled={busy}
              className="rounded-md bg-brand-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-brand-700 disabled:opacity-50"
            >
              {existing ? 'Save' : 'Create policy'}
            </button>
            {existing && (
              <>
                {(existing.selector === 'risk' || existing.selector === 'act') && existing.state === 'on' && (
                  <button type="button" disabled={busy} className={button} onClick={() => act(acceptPolicy)}>
                    Accept operations published since
                  </button>
                )}
                {existing.state !== 'expired' && (
                  <button type="button" disabled={busy} className={button} onClick={() => act(expirePolicy)}>
                    Expire now
                  </button>
                )}
                <button type="button" disabled={busy} className={button} onClick={() => act(revokePolicy)}>
                  Revoke
                </button>
              </>
            )}
          </div>
        )}
      </form>
    </div>
  )
}
