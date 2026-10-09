import type { StartedBy, TriggerKind } from './chat'
import { elapsedPhrase } from './elapsed'
import type { Schedule } from './schedules'
import type { Webhook } from './webhooks'

/** One schedule or webhook, as this panel says it. */
export type Trigger = {
  kind: TriggerKind
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

/**
 * What started a conversation, said the way its header says it.
 *
 * The kind outlives the trigger and the name does not, so a deleted one is
 * named for what it was rather than left blank.
 */
export function startedByPhrase(started: Pick<StartedBy, 'kind' | 'name'>): string {
  const kind = started.kind === 'schedule' ? 'schedule' : 'webhook'
  return started.name ? `the ${started.name} ${kind}` : `a ${kind} since deleted`
}

/**
 * The trigger an opening message came from, read from what was stored with it.
 *
 * A snapshot taken when it ran, so it still names a trigger renamed or deleted
 * since -- which is what an account of the past should do.
 */
export function triggerOf(metadata: Record<string, unknown>): Pick<StartedBy, 'kind' | 'name'> | null {
  if (metadata.schedule_id) {
    return { kind: 'schedule', name: (metadata.schedule_name as string | undefined) ?? null }
  }
  if (metadata.webhook_trigger_id) {
    return { kind: 'webhook', name: (metadata.webhook_trigger_name as string | undefined) ?? null }
  }
  return null
}
