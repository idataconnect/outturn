import { useCallback, useEffect, useMemo, useState } from 'react'
import { AlertTriangle, Clock, Plus, Trash2 } from 'lucide-react'
import cronstrue from 'cronstrue'

import { ApiError } from '../lib/api'
import {
  buildExpression,
  createSchedule,
  deleteSchedule,
  listSchedules,
  previewSchedule,
  readExpression,
  updateSchedule,
  type Recurrence,
  type Schedule,
} from '../lib/schedules'
import { useSession } from '../lib/session'

const field =
  'w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100'
const label = 'block text-sm text-surface-700 dark:text-surface-300 mb-1'

const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat']

const PRESETS: { kind: Recurrence; label: string }[] = [
  { kind: 'daily', label: 'Daily' },
  { kind: 'weekdays', label: 'Weekdays' },
  { kind: 'weekly', label: 'Weekly' },
  { kind: 'monthly', label: 'Monthly' },
  { kind: 'custom', label: 'Custom' },
]

/** The expression in words, or the expression itself if it cannot be read. */
function inWords(expression: string): string {
  try {
    return cronstrue.toString(expression, { verbose: false })
  } catch {
    return expression
  }
}

function when(iso: string, timezone: string): string {
  return new Date(iso).toLocaleString(undefined, {
    weekday: 'short',
    day: 'numeric',
    month: 'short',
    hour: 'numeric',
    minute: '2-digit',
    timeZone: timezone,
    timeZoneName: 'short',
  })
}

function ago(iso: string): string {
  const seconds = Math.round((Date.now() - new Date(iso).getTime()) / 1000)
  if (seconds < 90) return 'just now'
  const minutes = Math.round(seconds / 60)
  if (minutes < 90) return `${minutes} minutes ago`
  const hours = Math.round(minutes / 60)
  if (hours < 36) return `${hours} hours ago`
  return `${Math.round(hours / 24)} days ago`
}

type Draft = {
  id: string | null
  name: string
  prompt: string
  kind: Recurrence
  hour: number
  minute: number
  weekdays: number[]
  dayOfMonth: number
  custom: string
  timezone: string
  enabled: boolean
}

function blank(): Draft {
  return {
    id: null,
    name: '',
    prompt: '',
    kind: 'daily',
    hour: 9,
    minute: 0,
    weekdays: [1],
    dayOfMonth: 1,
    custom: '0 9 * * *',
    // The browser's zone, which is right for whoever is creating it. A
    // workspace default would be better for somebody in another office
    // editing it later; that is a settings-cascade decision and is not
    // defaulted into here.
    timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC',
    enabled: true,
  }
}

function fromSchedule(s: Schedule): Draft {
  const parsed = readExpression(s.expression)
  return {
    id: s.id,
    name: s.name,
    prompt: s.prompt,
    kind: parsed.kind,
    hour: parsed.hour,
    minute: parsed.minute,
    weekdays: parsed.weekdays,
    dayOfMonth: parsed.dayOfMonth,
    custom: s.expression,
    timezone: s.timezone,
    enabled: s.enabled,
  }
}

/**
 * What an agent does on its own.
 *
 * A tab on the agent rather than a page of its own: a schedule without an
 * agent means nothing, and the question people have is "what does this agent
 * do when nobody is here".
 */
export default function AgentSchedules({ agentId }: { agentId: string }) {
  const state = useSession()
  const [rows, setRows] = useState<Schedule[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [draft, setDraft] = useState<Draft | null>(null)
  const [saving, setSaving] = useState(false)
  const [preview, setPreview] = useState<{ upcoming: string[]; problem?: string } | null>(null)

  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canEdit = authorities.includes('agents:update')

  const reload = useCallback(async () => {
    try {
      setRows(await listSchedules(agentId))
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to load schedules')
    } finally {
      setLoading(false)
    }
  }, [agentId])

  useEffect(() => {
    void reload()
  }, [reload])

  const expression = useMemo(
    () =>
      draft
        ? buildExpression(
            draft.kind,
            draft.hour,
            draft.minute,
            draft.weekdays,
            draft.dayOfMonth,
            draft.custom,
          )
        : '',
    [draft],
  )

  // Asked of the server while somebody is still typing, because seeing the
  // actual dates is what catches a wrong expression before it costs a day
  // rather than after. Debounced so a keystroke in the custom field does not
  // become a request each.
  useEffect(() => {
    if (!draft || !expression) {
      setPreview(null)
      return
    }
    const timer = setTimeout(() => {
      void previewSchedule(expression, draft.timezone)
        .then(setPreview)
        .catch(() => setPreview(null))
    }, 250)
    return () => clearTimeout(timer)
  }, [expression, draft?.timezone, draft])

  async function save() {
    if (!draft) return
    setSaving(true)
    try {
      const input = {
        agent_id: agentId,
        name: draft.name,
        prompt: draft.prompt,
        expression,
        timezone: draft.timezone,
        enabled: draft.enabled,
      }
      if (draft.id) await updateSchedule(draft.id, input)
      else await createSchedule(input)
      setDraft(null)
      await reload()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to save')
    } finally {
      setSaving(false)
    }
  }

  async function toggle(s: Schedule) {
    try {
      await updateSchedule(s.id, {
        agent_id: s.agent_id,
        name: s.name,
        prompt: s.prompt,
        expression: s.expression,
        timezone: s.timezone,
        enabled: !s.enabled,
      })
      await reload()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to save')
    }
  }

  async function remove(s: Schedule) {
    if (!confirm(`Delete "${s.name}"? Sessions it already produced are kept.`)) return
    try {
      await deleteSchedule(s.id)
      await reload()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to delete')
    }
  }

  if (loading) return <p className="text-sm text-surface-500">Loading…</p>

  return (
    <section className="mt-8 space-y-4">
      <div className="flex items-baseline justify-between gap-4">
        <div>
          <h2 className="text-lg font-semibold text-surface-900 dark:text-surface-100">
            Schedules
          </h2>
          <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
            Turns this agent starts on its own. Nobody is waiting for the reply, so these run
            behind anything a person is watching.
          </p>
        </div>
        {canEdit && !draft && (
          <button
            type="button"
            onClick={() => setDraft(blank())}
            className="shrink-0 inline-flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-md bg-brand-600 text-white hover:bg-brand-700"
          >
            <Plus className="w-4 h-4" /> New schedule
          </button>
        )}
      </div>

      {error && (
        <p className="text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      {rows.length === 0 && !draft && (
        <p className="text-sm text-surface-500">
          None yet. A schedule might summarise yesterday's work each morning, or check
          something overnight.
        </p>
      )}

      <ul className="space-y-2">
        {rows.map((s) => (
          <li
            key={s.id}
            className="p-3 rounded-lg border border-surface-200 dark:border-surface-700 bg-white dark:bg-surface-800"
          >
            <div className="flex items-start justify-between gap-3">
              <button
                type="button"
                onClick={() => canEdit && setDraft(fromSchedule(s))}
                disabled={!canEdit}
                className="text-left min-w-0 flex-1 disabled:cursor-default"
              >
                <div className="flex items-center gap-2">
                  <span className="font-medium text-surface-900 dark:text-surface-100">
                    {s.name}
                  </span>
                  {!s.enabled && (
                    <span className="text-xs px-1.5 py-0.5 rounded bg-surface-200 dark:bg-surface-700 text-surface-600 dark:text-surface-400">
                      off
                    </span>
                  )}
                </div>
                {/* The recurrence in words. The expression itself never
                    appears in the list: it is exact and unreadable. */}
                <p className="text-sm text-surface-600 dark:text-surface-400">
                  {inWords(s.expression)} · {s.timezone}
                </p>
                <p className="text-sm text-surface-500 truncate">{s.prompt}</p>
              </button>

              {canEdit && (
                <div className="flex items-center gap-1 shrink-0">
                  <button
                    type="button"
                    onClick={() => void toggle(s)}
                    className="px-2 py-1 text-xs rounded border border-surface-300 dark:border-surface-600 hover:bg-surface-100 dark:hover:bg-surface-700"
                  >
                    {s.enabled ? 'Turn off' : 'Turn on'}
                  </button>
                  <button
                    type="button"
                    onClick={() => void remove(s)}
                    aria-label={`Delete ${s.name}`}
                    className="p-1.5 rounded text-surface-500 hover:text-red-600 hover:bg-surface-100 dark:hover:bg-surface-700"
                  >
                    <Trash2 className="w-4 h-4" />
                  </button>
                </div>
              )}
            </div>

            <div className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-surface-500">
              {s.enabled && s.upcoming[0] && (
                <span className="inline-flex items-center gap-1">
                  <Clock className="w-3.5 h-3.5" />
                  Next {when(s.upcoming[0], s.timezone)}
                </span>
              )}
              {s.last_run_at && (
                <span className={s.last_status === 'failed' ? 'text-red-600 dark:text-red-400' : ''}>
                  Last run {ago(s.last_run_at)}
                  {s.last_status === 'failed' ? ' · failed' : ''}
                </span>
              )}
              {/* A schedule that quietly missed a week looks exactly like one
                  that never worked, so the skip is said out loud. */}
              {s.skipped > 0 && <span>{s.skipped} firing(s) skipped</span>}
            </div>

            {(s.problem || s.last_error) && (
              <p className="mt-2 text-xs text-red-600 dark:text-red-400 flex items-start gap-1.5">
                <AlertTriangle className="w-3.5 h-3.5 mt-0.5 shrink-0" />
                {s.problem ?? s.last_error}
              </p>
            )}
          </li>
        ))}
      </ul>

      {draft && (
        <div className="p-4 rounded-lg border border-surface-200 dark:border-surface-700 bg-surface-50 dark:bg-surface-800/50 space-y-4">
          <div>
            <label className={label} htmlFor="sched-name">
              Name
            </label>
            <input
              id="sched-name"
              className={field}
              value={draft.name}
              onChange={(e) => setDraft({ ...draft, name: e.target.value })}
              placeholder="Morning summary"
            />
          </div>

          {/* First, because it is the thing somebody came here to write.
              Everything below is a decision they have to be walked into. */}
          <div>
            <label className={label} htmlFor="sched-prompt">
              What should it do?
            </label>
            <textarea
              id="sched-prompt"
              className={`${field} min-h-24`}
              value={draft.prompt}
              onChange={(e) => setDraft({ ...draft, prompt: e.target.value })}
              placeholder="Summarise yesterday's bookings and note anything unusual."
            />
            <p className="mt-1 text-xs text-surface-500">
              Stored as the first message of a new conversation each time it runs. It is shown
              as the schedule's words, not as something a person said.
            </p>
          </div>

          <div>
            <span className={label}>When?</span>
            <div className="flex flex-wrap gap-1">
              {PRESETS.map((p) => (
                <button
                  key={p.kind}
                  type="button"
                  onClick={() => setDraft({ ...draft, kind: p.kind })}
                  className={`px-3 py-1.5 text-sm rounded-md border ${
                    draft.kind === p.kind
                      ? 'bg-brand-600 text-white border-brand-600'
                      : 'border-surface-300 dark:border-surface-600 hover:bg-surface-100 dark:hover:bg-surface-700'
                  }`}
                >
                  {p.label}
                </button>
              ))}
            </div>
          </div>

          {draft.kind === 'weekly' && (
            <div className="flex flex-wrap gap-1">
              {DAYS.map((d, i) => (
                <button
                  key={d}
                  type="button"
                  onClick={() =>
                    setDraft({
                      ...draft,
                      weekdays: draft.weekdays.includes(i)
                        ? draft.weekdays.filter((w) => w !== i)
                        : [...draft.weekdays, i],
                    })
                  }
                  className={`px-2.5 py-1 text-sm rounded border ${
                    draft.weekdays.includes(i)
                      ? 'bg-brand-600 text-white border-brand-600'
                      : 'border-surface-300 dark:border-surface-600'
                  }`}
                >
                  {d}
                </button>
              ))}
            </div>
          )}

          {draft.kind === 'monthly' && (
            <div>
              <label className={label} htmlFor="sched-dom">
                Day of the month
              </label>
              <select
                id="sched-dom"
                className={field}
                value={draft.dayOfMonth}
                onChange={(e) => setDraft({ ...draft, dayOfMonth: Number(e.target.value) })}
              >
                {Array.from({ length: 31 }, (_, i) => i + 1).map((d) => (
                  <option key={d} value={d}>
                    {d}
                  </option>
                ))}
              </select>
              {draft.dayOfMonth > 28 && (
                <p className="mt-1 text-xs text-surface-500">
                  Months without this day are skipped.
                </p>
              )}
            </div>
          )}

          {draft.kind === 'custom' ? (
            <div>
              <label className={label} htmlFor="sched-cron">
                Cron expression
              </label>
              <input
                id="sched-cron"
                className={`${field} font-mono text-sm`}
                value={draft.custom}
                onChange={(e) => setDraft({ ...draft, custom: e.target.value })}
                placeholder="0 9 * * 1-5"
              />
              <p className="mt-1 text-xs text-surface-500">minute hour day month weekday</p>
            </div>
          ) : (
            <div className="grid grid-cols-2 gap-3">
              <div>
                <label className={label} htmlFor="sched-time">
                  Time
                </label>
                <input
                  id="sched-time"
                  type="time"
                  className={field}
                  value={`${String(draft.hour).padStart(2, '0')}:${String(draft.minute).padStart(2, '0')}`}
                  onChange={(e) => {
                    const [h, m] = e.target.value.split(':').map(Number)
                    setDraft({ ...draft, hour: h || 0, minute: m || 0 })
                  }}
                />
              </div>
              <div>
                <label className={label} htmlFor="sched-tz">
                  Timezone
                </label>
                <select
                  id="sched-tz"
                  className={field}
                  value={draft.timezone}
                  onChange={(e) => setDraft({ ...draft, timezone: e.target.value })}
                >
                  {(Intl.supportedValuesOf?.('timeZone') ?? [draft.timezone]).map((tz) => (
                    <option key={tz} value={tz}>
                      {tz}
                    </option>
                  ))}
                </select>
              </div>
            </div>
          )}

          {/* The most useful thing on the page: an expression is exact and
              unreadable, and dates are the only form in which a mistake is
              obvious before it has cost a day. */}
          <div className="p-3 rounded-md bg-white dark:bg-surface-800 border border-surface-200 dark:border-surface-700">
            {preview?.problem ? (
              <p className="text-sm text-red-600 dark:text-red-400 flex items-start gap-1.5">
                <AlertTriangle className="w-4 h-4 mt-0.5 shrink-0" />
                {preview.problem}
              </p>
            ) : preview?.upcoming.length ? (
              <>
                <p className="text-sm text-surface-700 dark:text-surface-300">
                  {inWords(expression)}
                </p>
                <ul className="mt-1.5 space-y-0.5">
                  {preview.upcoming.slice(0, 3).map((at) => (
                    <li key={at} className="text-xs text-surface-500">
                      {when(at, draft.timezone)}
                    </li>
                  ))}
                </ul>
              </>
            ) : (
              <p className="text-sm text-surface-500">Checking…</p>
            )}
          </div>

          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={() => void save()}
              disabled={saving || !draft.name.trim() || !draft.prompt.trim() || !!preview?.problem}
              className="px-4 py-2 text-sm rounded-md bg-brand-600 text-white hover:bg-brand-700 disabled:opacity-50"
            >
              {saving ? 'Saving…' : draft.id ? 'Save' : 'Create'}
            </button>
            <button
              type="button"
              onClick={() => setDraft(null)}
              className="px-4 py-2 text-sm rounded-md border border-surface-300 dark:border-surface-600 hover:bg-surface-100 dark:hover:bg-surface-700"
            >
              Cancel
            </button>
          </div>
        </div>
      )}
    </section>
  )
}
