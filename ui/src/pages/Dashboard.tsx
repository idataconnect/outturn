import { useEffect, useMemo, useState } from 'react'
import { AlertTriangle, Table2 } from 'lucide-react'

import { ApiError, api } from '../lib/api'
import { useSession } from '../lib/session'
import { TOKEN_KINDS, compact, dayLabel, exact, measured, share } from '../lib/viz'
import UsageArea, { type Bucket } from '../components/UsageArea'
import UsageRanked, { type Slice } from '../components/UsageRanked'
import StatTile from '../components/StatTile'

type Summary = {
  from: string
  to: string
  totals: {
    calls: number
    prompt_tokens: number
    completion_tokens: number
    cache_read_tokens: number
    cache_write_tokens: number
    reasoning_tokens: number
    sessions: number
    agents: number
    workspaces: number
  }
  daily: Bucket[]
  by_workspace: Slice[]
  by_model: Slice[]
  by_agent: Slice[]
  by_account: Slice[]
  by_source: Slice[]
  by_traffic: Slice[]
}

/** The windows a reader can ask for, in days. */
const WINDOWS = [
  { days: 7, label: '7 days' },
  { days: 30, label: '30 days' },
  { days: 90, label: '90 days' },
] as const

/**
 * What the platform did, over a window.
 *
 * Everything here is a reading of the usage ledger, which is the only thing
 * that records model calls. It is therefore about spend and volume, and says
 * nothing about turns that never reached a model -- a distinction worth keeping
 * in mind before this grows panels that look like they cover everything.
 *
 * Tokens throughout, never money: the ledger stores tokens for the reason
 * docs/usage.md gives, and a dashboard that multiplied them by a rate card
 * would be inventing the one number people argue about.
 */
export default function Dashboard() {
  const state = useSession()
  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canRead = authorities.includes('usage:read')
  const isOperator =
    state.status === 'authenticated' && state.session.roles.includes('system_admin')
  const workspaceId = state.status === 'authenticated' ? state.session.workspace_id : null

  const [days, setDays] = useState<number>(30)
  // Everyone, operator included, lands on their own workspace. Widening to the
  // platform is a deliberate click rather than a default: a page that showed
  // every workspace's figures the moment an operator opened it would widen
  // itself without being asked, and there is a test holding it to that.
  const [everywhere, setEverywhere] = useState(false)
  const [summary, setSummary] = useState<Summary | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [table, setTable] = useState(false)

  useEffect(() => {
    if (!canRead) {
      setLoading(false)
      return
    }
    let current = true
    setLoading(true)
    void (async () => {
      // The window ends at the next midnight so today is a whole bucket, and
      // starts `days` before that. Computed here as well as defaulted in the
      // API because the picker has to be able to ask for a window the default
      // is not.
      const end = new Date()
      end.setUTCHours(0, 0, 0, 0)
      end.setUTCDate(end.getUTCDate() + 1)
      const start = new Date(end)
      start.setUTCDate(start.getUTCDate() - days)

      const scope = everywhere && isOperator ? '&scope=all' : ''
      try {
        const data = await api<Summary>(
          `/v1/usage/summary?from=${start.toISOString()}&to=${end.toISOString()}${scope}`,
        )
        if (current) {
          setSummary(data)
          setError(null)
        }
      } catch (e) {
        if (current) {
          setError(e instanceof ApiError ? e.message : 'failed to load usage')
          setSummary(null)
        }
      } finally {
        if (current) setLoading(false)
      }
    })()
    return () => {
      current = false
    }
  }, [days, everywhere, isOperator, canRead, workspaceId])

  const totals = summary?.totals
  // Summed over the one list of kinds rather than by naming five fields here,
  // so this total and the chart below it can never come to cover different
  // things. The API sums the same five columns for a slice's `tokens`.
  const allTokens = totals
    ? TOKEN_KINDS.reduce((sum, kind) => sum + (totals[kind.key] ?? 0), 0)
    : 0

  // How much of the window nobody actually measured. Shown rather than folded
  // in, because a total that mixes reported and unmeasured rows without saying
  // so presents an estimate as a fact -- the same reason the ledger carries
  // `usage_source` at all.
  const unmeasured = useMemo(() => {
    const rows = summary?.by_source ?? []
    // Anything not named as measured, rather than the two values that mean
    // unmeasured today -- see `measured`. A source this page has never heard of
    // belongs in the caveat, not silently in the total.
    const suspect = rows.filter((r) => !measured(r.key))
    return {
      tokens: suspect.reduce((sum, r) => sum + r.tokens, 0),
      calls: suspect.reduce((sum, r) => sum + r.calls, 0),
    }
  }, [summary])

  if (!canRead) {
    return (
      <div className="p-6">
        <h1 className="text-2xl font-semibold text-surface-900 dark:text-surface-100">Dashboard</h1>
        <p className="mt-2 text-surface-600 dark:text-surface-400">
          {state.status === 'authenticated'
            ? `Signed in as ${state.displayName}. Reading usage needs the usage:read authority.`
            : 'Overview coming soon.'}
        </p>
      </div>
    )
  }

  return (
    <div className="p-6 max-w-6xl space-y-6">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold text-surface-900 dark:text-surface-100">
            Dashboard
          </h1>
          <p className="mt-2 text-surface-600 dark:text-surface-400">
            Every model call in the window, from the usage ledger.{' '}
            {everywhere ? 'Across every workspace.' : 'This workspace only.'}
          </p>
        </div>

        {/* Filters in one row above the charts. */}
        <div className="flex items-center gap-2">
          {isOperator && (
            <div
              className="flex rounded-md border border-surface-200 dark:border-surface-700 overflow-hidden"
              role="group"
              aria-label="Scope"
            >
              {[
                { on: false, label: 'This workspace' },
                { on: true, label: 'Platform' },
              ].map((option) => (
                <button
                  key={option.label}
                  type="button"
                  onClick={() => setEverywhere(option.on)}
                  aria-pressed={everywhere === option.on}
                  className={`px-3 py-1.5 text-sm ${
                    everywhere === option.on
                      ? 'bg-brand-700 dark:bg-brand-600 text-white font-medium'
                      : 'text-surface-600 dark:text-surface-300 hover:bg-surface-100 dark:hover:bg-surface-800'
                  }`}
                >
                  {option.label}
                </button>
              ))}
            </div>
          )}

          <div
            className="flex rounded-md border border-surface-200 dark:border-surface-700 overflow-hidden"
            role="group"
            aria-label="Window"
          >
            {WINDOWS.map((window) => (
              <button
                key={window.days}
                type="button"
                onClick={() => setDays(window.days)}
                aria-pressed={days === window.days}
                className={`px-3 py-1.5 text-sm ${
                  days === window.days
                    ? 'bg-brand-700 dark:bg-brand-600 text-white font-medium'
                    : 'text-surface-600 dark:text-surface-300 hover:bg-surface-100 dark:hover:bg-surface-800'
                }`}
              >
                {window.label}
              </button>
            ))}
          </div>
        </div>
      </div>

      {error && (
        <p className="text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      {loading && !summary ? (
        <p className="text-sm text-surface-600 dark:text-surface-400">Loading…</p>
      ) : !summary ? null : totals && totals.calls === 0 ? (
        // Nothing happened. Said plainly rather than as a grid of zeros, which
        // reads as a broken page rather than a quiet month.
        <div className="rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 p-10 text-center">
          <p className="text-sm font-medium text-surface-900 dark:text-surface-100">
            No model calls in this window.
          </p>
          <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
            Every figure here comes from the usage ledger, which gets a row when an agent
            calls a model. Start a conversation and this fills in.
          </p>
        </div>
      ) : (
        <>
          {/* The hero figure: the one number the page leads with. */}
          <section className="rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 p-5">
            <p className="text-xs font-medium uppercase tracking-wide text-surface-500 dark:text-surface-400">
              Tokens in the last {days} days
            </p>
            <p
              className="mt-1 text-5xl font-semibold text-surface-900 dark:text-surface-100"
              title={exact(allTokens)}
            >
              {compact(allTokens)}
            </p>
            <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
              across {exact(totals!.calls)} model {totals!.calls === 1 ? 'call' : 'calls'}
              {everywhere && ` in ${exact(totals!.workspaces)} workspaces`}
              {' · '}
              {dayLabel(summary.from)} to {dayLabel(summary.daily.at(-1)?.at ?? summary.from)}
            </p>

            {unmeasured.calls > 0 && (
              // Not an error, so not red: it is a caveat about precision, and
              // it carries an icon and words rather than resting on colour.
              <p className="mt-3 flex items-start gap-2 text-xs text-amber-700 dark:text-amber-400">
                <AlertTriangle size={14} className="mt-0.5 shrink-0" aria-hidden />
                <span>
                  {share(unmeasured.tokens, allTokens)} of these tokens come from{' '}
                  {exact(unmeasured.calls)} {unmeasured.calls === 1 ? 'call' : 'calls'} the
                  provider did not report. Counted here, but not a figure to bill from.
                </span>
              </p>
            )}
          </section>

          <section className="grid gap-4 grid-cols-2 lg:grid-cols-4">
            <StatTile label="Model calls" value={totals!.calls} hint="one per round" />
            <StatTile label="Sessions" value={totals!.sessions} hint="conversations touched" />
            <StatTile label="Agents" value={totals!.agents} hint="that answered" />
            <StatTile
              label="Cache reads"
              value={totals!.cache_read_tokens}
              hint={`${share(totals!.cache_read_tokens, allTokens)} of tokens`}
            />
          </section>

          <section className="rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 p-5">
            <div className="flex items-start justify-between gap-4">
              <div>
                <h2 className="text-sm font-semibold text-surface-900 dark:text-surface-100">
                  Tokens per day
                </h2>
                <p className="mt-0.5 text-xs text-surface-500 dark:text-surface-400">
                  Stacked by kind. Point at a day for its figures.
                </p>
              </div>
              <button
                type="button"
                onClick={() => setTable((v) => !v)}
                aria-pressed={table}
                className="flex items-center gap-1.5 px-2.5 py-1.5 rounded-md text-xs text-surface-600 dark:text-surface-300 hover:bg-surface-100 dark:hover:bg-surface-800"
              >
                <Table2 size={14} aria-hidden />
                {table ? 'Chart' : 'Table'}
              </button>
            </div>

            <div className="mt-4">
              {table ? (
                // The table view the chart's accessibility rests on: every
                // figure the chart encodes, readable without colour or hover.
                <div className="max-h-80 overflow-auto">
                  <table className="w-full text-sm">
                    <thead className="sticky top-0 bg-white dark:bg-surface-900">
                      <tr className="text-left text-xs text-surface-500 dark:text-surface-400">
                        <th className="py-1.5 pr-3 font-medium">Day</th>
                        <th className="py-1.5 px-3 font-medium text-right">Calls</th>
                        {TOKEN_KINDS.map((kind) => (
                          <th key={kind.key} className="py-1.5 px-3 font-medium text-right">
                            {kind.label}
                          </th>
                        ))}
                      </tr>
                    </thead>
                    <tbody
                      className="divide-y divide-surface-100 dark:divide-surface-800"
                      style={{ fontVariantNumeric: 'tabular-nums' }}
                    >
                      {summary.daily.map((bucket) => (
                        <tr key={bucket.at} className="text-surface-800 dark:text-surface-200">
                          <td className="py-1.5 pr-3 whitespace-nowrap">{dayLabel(bucket.at)}</td>
                          <td className="py-1.5 px-3 text-right">{exact(bucket.calls)}</td>
                          {TOKEN_KINDS.map((kind) => (
                            <td key={kind.key} className="py-1.5 px-3 text-right">
                              {exact(bucket[kind.key])}
                            </td>
                          ))}
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              ) : (
                <UsageArea buckets={summary.daily} />
              )}
            </div>
          </section>

          <section className="grid gap-4 md:grid-cols-2">
            {everywhere && (
              <Panel
                title="Workspaces"
                subtitle="Tokens, largest first"
                slices={summary.by_workspace}
                empty="No workspace spent anything in this window."
              />
            )}
            <Panel
              title="Models"
              subtitle="The model that actually answered"
              slices={summary.by_model}
              empty="No model answered in this window."
            />
            <Panel
              title="Agents"
              subtitle="Tokens, largest first"
              slices={summary.by_agent}
              empty="No agent answered in this window."
              // A session cannot exist without an agent, and everything done
              // for one -- including the platform's own naming and compaction
              // -- bills to it. So a null here is a row that lost its
              // attribution rather than one that never had any, and it should
              // read as the anomaly it is.
              unattributed="Missing an agent"
            />
            <Panel
              title="Accounts"
              subtitle="The workspace's own label for whose conversation this was"
              slices={summary.by_account}
              empty="No conversation in this window carried an account label."
              unattributed="No account label"
            />
            <Panel
              title="Work"
              subtitle="An agent answering somebody, against the platform's own naming and compaction"
              slices={summary.by_traffic}
              empty="No work in this window."
            />
          </section>
        </>
      )}
    </div>
  )
}

function Panel({
  title,
  subtitle,
  slices,
  empty,
  unattributed,
}: {
  title: string
  subtitle: string
  slices: Slice[]
  empty: string
  unattributed?: string
}) {
  return (
    <div className="rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 p-5">
      <h2 className="text-sm font-semibold text-surface-900 dark:text-surface-100">{title}</h2>
      <p className="mt-0.5 mb-4 text-xs text-surface-500 dark:text-surface-400">{subtitle}</p>
      <UsageRanked slices={slices} empty={empty} unattributed={unattributed} />
    </div>
  )
}
