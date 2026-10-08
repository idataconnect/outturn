import { elapsedPhrase } from './elapsed'
import type { Schedule } from './schedules'
import type { Webhook } from './webhooks'

/** One schedule or webhook, as this panel says it. */
export type Trigger = {
  kind: 'schedule' | 'webhook'
  id: string
  name: string
  agentId: string
  enabled: boolean
  /** Why it is not working, when it is not. */
  trouble: string | null
  /** What it does next or did last, when it is working. */
  status: string
  /** When it next runs, for ordering; a webhook has no such time. */
  next: number | null
}

const ago = (iso: string) => elapsedPhrase(new Date(iso).getTime()).toLowerCase()

function when(iso: string, timezone: string): string {
  return new Date(iso).toLocaleString(undefined, {
    weekday: 'short',
    hour: 'numeric',
    minute: '2-digit',
    timeZone: timezone,
  })
}

export function fromSchedule(s: Schedule): Trigger {
  const trouble =
    s.problem ?? (s.last_status === 'failed' ? (s.last_error ?? 'The last run failed') : null)
  const next = s.enabled && s.upcoming[0] ? s.upcoming[0] : null
  return {
    kind: 'schedule',
    id: s.id,
    name: s.name,
    agentId: s.agent_id,
    enabled: s.enabled,
    trouble,
    status: !s.enabled
      ? 'Off'
      : next
        ? `Next ${when(next, s.timezone)}`
        : s.last_run_at
          ? `Last ran ${ago(s.last_run_at)}`
          : 'Not run yet',
    next: next ? new Date(next).getTime() : null,
  }
}

export function fromWebhook(w: Webhook): Trigger {
  const trouble =
    w.last_status === 'failed'
      ? (w.last_error ?? 'The last request failed')
      : w.last_status === 'refused'
        ? `The last request was refused${w.last_error ? `: ${w.last_error}` : ''}`
        : null
  return {
    kind: 'webhook',
    id: w.id,
    name: w.name,
    agentId: w.agent_id,
    enabled: w.enabled,
    trouble,
    status: !w.enabled
      ? 'Off'
      : w.last_at
        ? `Last request ${ago(w.last_at)}`
        : 'No requests yet',
    next: null,
  }
}

/**
 * Trouble first, then what is on, then what is off. Among what is on, the
 * soonest schedule leads and webhooks follow, since a webhook runs when it is
 * called and has no "next" to rank by.
 */
export function ordered(triggers: Trigger[]): Trigger[] {
  const rank = (t: Trigger) => (t.trouble ? 0 : t.enabled ? 1 : 2)
  return [...triggers].sort(
    (a, b) =>
      rank(a) - rank(b) ||
      (a.next ?? Infinity) - (b.next ?? Infinity) ||
      a.name.localeCompare(b.name),
  )
}
