import { useEffect, useRef, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router'

import { ApiError, allPages } from '../lib/api'
import { createOneLine, decimalsOf, kindOf, type BandsPage } from '../lib/autoApproval'
import { currencyMark, toMajor, toMinor } from '../lib/money'
import { paths } from '../lib/paths'

const field =
  'rounded-md border border-surface-300 bg-white px-2 py-1 text-sm dark:border-surface-700 dark:bg-surface-900'
const label = 'block text-xs font-medium text-surface-700 dark:text-surface-300'
const hint = 'text-xs text-surface-600 dark:text-surface-400'

/** The counts the API gives a one-line policy; the full editor changes them. */
const PER_TURN = 1
const PER_DAY = 20

type Named = { id: string; name: string }

/**
 * The common case in one line: "Charges under £50 go through automatically, up
 * to £500 a day" -- for an agent, until a date.
 *
 * One write: the API sets the low band of every operation of that kind in
 * that currency, where it is not that already, and creates a policy for the
 * kind up to low risk with that daily total -- both, or neither, so a band the
 * floor refuses leaves no policy behind.
 *
 * Prefilled from the address -- `line` (the kind), `currency` and `low` in the
 * currency's standard minor units -- which is how an approval's offer opens it.
 */
export default function OneLinePolicy({ bands, onChanged }: { bands: BandsPage; onChanged: () => void }) {
  const [params] = useSearchParams()
  const navigate = useNavigate()
  const kinds = [...new Set(bands.operations.map((o) => o.requires))]
  const asked = params.get('line')

  const [requires, setRequires] = useState(() => (asked && kinds.includes(asked) ? asked : (kinds[0] ?? '')))
  const currencies = [...new Set(bands.operations.filter((o) => o.requires === requires).map((o) => o.currency))]
  const [currency, setCurrency] = useState(() => {
    const c = params.get('currency')
    return c && currencies.includes(c) ? c : (currencies[0] ?? '')
  })
  const ops = bands.operations.filter((o) => o.requires === requires && o.currency === currency)
  // Both figures are sent in the currency's standard units, whatever its
  // operations count in; the API puts each into an operation's own.
  const minorUnits = decimalsOf(bands)[currency] ?? 2

  const [low, setLow] = useState(() => {
    const given = Number(params.get('low'))
    if (params.get('low') && Number.isInteger(given) && given > 0) return toMajor(given, minorUnits)
    const now = ops[0]
    return now?.effective.low ? toMajor(now.effective.low.amount, now.minor_units) : ''
  })
  const [daily, setDaily] = useState('')
  const [agentId, setAgentId] = useState('')
  const [agents, setAgents] = useState<Named[]>([])
  const [mode, setMode] = useState<'trial' | 'on'>('trial')
  const [expires, setExpires] = useState(() => {
    const d = new Date()
    d.setDate(d.getDate() + 90)
    return d.toISOString().slice(0, 10)
  })
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    void allPages<Named>('/v1/agents').then(setAgents).catch(() => setAgents([]))
  }, [])

  // Arriving from an approval's offer, the form is what the reader came for.
  const ref = useRef<HTMLFormElement>(null)
  useEffect(() => {
    if (asked) ref.current?.scrollIntoView?.({ block: 'center' })
  }, [asked])

  if (kinds.length === 0) return null

  const lowMinor = toMinor(low, minorUnits)
  const dailyMinor = toMinor(daily, minorUnits)

  async function save(e: React.FormEvent) {
    e.preventDefault()
    setError(null)
    if (lowMinor === null || lowMinor === 0) {
      setError(`the amount requests go through under is a figure in ${currency} above zero`)
      return
    }
    if (dailyMinor === null) {
      setError(`the daily total is a figure in ${currency}`)
      return
    }
    setBusy(true)
    try {
      const policy = await createOneLine({
        requires,
        currency,
        low: lowMinor,
        daily_total: dailyMinor,
        agent_id: agentId || null,
        mode,
        // The end of the chosen day, as the full editor has it.
        expires_at: new Date(`${expires}T23:59:59`).toISOString(),
      })
      onChanged()
      navigate(paths.autoApproval(policy.id))
    } catch (err) {
      // A band past the floor is refused with its reason, and nothing is set.
      setError(err instanceof ApiError ? err.message : 'the policy could not be set')
      setBusy(false)
    }
  }

  const mark = currencyMark(currency)
  return (
    <form
      ref={ref}
      id="one-line"
      aria-label="One-line policy"
      onSubmit={save}
      className="mt-6 rounded-md border border-surface-200 p-4 dark:border-surface-800"
    >
      <fieldset disabled={busy} className="space-y-3">
        <p className="flex flex-wrap items-center gap-x-2 gap-y-2 text-sm text-surface-900 dark:text-surface-100">
          {kinds.length > 1 ? (
            <select
              aria-label="Kind"
              className={field}
              value={requires}
              onChange={(e) => {
                setRequires(e.target.value)
                const first = bands.operations.find((o) => o.requires === e.target.value)
                setCurrency(first?.currency ?? '')
              }}
            >
              {kinds.map((k) => (
                <option key={k} value={k}>
                  {kindOf(k)}
                </option>
              ))}
            </select>
          ) : (
            <span className="font-medium">{kindOf(requires)}</span>
          )}
          <span>under</span>
          <span className="inline-flex items-center gap-1">
            {currencies.length > 1 ? (
              <select aria-label="Currency" className={field} value={currency} onChange={(e) => setCurrency(e.target.value)}>
                {currencies.map((c) => (
                  <option key={c} value={c}>
                    {c}
                  </option>
                ))}
              </select>
            ) : (
              <span aria-hidden>{mark}</span>
            )}
            <input
              aria-label="Under"
              className={`${field} w-24`}
              inputMode="decimal"
              value={low}
              onChange={(e) => setLow(e.target.value)}
            />
          </span>
          <span>go through automatically, up to</span>
          <span className="inline-flex items-center gap-1">
            <span aria-hidden>{mark}</span>
            <input
              aria-label="A day"
              className={`${field} w-24`}
              inputMode="decimal"
              value={daily}
              onChange={(e) => setDaily(e.target.value)}
            />
          </span>
          <span>a day</span>
        </p>

        <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
          <label className={label}>
            Agent
            <select className={`${field} mt-1 block w-full`} value={agentId} onChange={(e) => setAgentId(e.target.value)}>
              <option value="">Every agent in this workspace</option>
              {agents.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.name}
                </option>
              ))}
            </select>
          </label>
          <label className={label}>
            Until
            <input
              type="date"
              className={`${field} mt-1 block w-full`}
              value={expires}
              onChange={(e) => setExpires(e.target.value)}
            />
          </label>
          <label className={label}>
            Mode
            <select
              className={`${field} mt-1 block w-full`}
              value={mode}
              onChange={(e) => setMode(e.target.value as 'trial' | 'on')}
            >
              <option value="trial">Trial: people are still asked</option>
              <option value="on">On: covered requests go out without asking</option>
            </select>
          </label>
        </div>

        <p className={hint}>
          Sets the low band of {ops.length === 1 ? 'its operation' : `its ${ops.length} operations`} in {currency}{' '}
          where it is not that already.
          At most {PER_TURN} a turn and {PER_DAY} a day; the full editor changes either, and anything else.
        </p>

        {error && (
          <p role="alert" className="text-sm text-red-600 dark:text-red-400">
            {error}
          </p>
        )}

        <button
          type="submit"
          className="rounded-md bg-brand-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-brand-700 disabled:opacity-50"
        >
          {busy ? 'Setting…' : 'Create policy'}
        </button>
      </fieldset>
    </form>
  )
}
