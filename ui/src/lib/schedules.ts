import { api } from './api'

/**
 * A schedule is a turn that starts because the clock said so.
 *
 * The expression is storage, not interface: it is exact and nobody reads one
 * correctly, so the editor offers the ordinary shapes and shows `upcoming` as
 * dates. Those come from the server rather than being computed here, so what
 * is shown is what will actually fire -- a second implementation in the
 * browser is a second set of rules that can disagree, and the one that matters
 * is the one the firing loop uses.
 */
export type Schedule = {
  id: string
  workspace_id: string
  agent_id: string
  name: string
  prompt: string
  expression: string
  /** IANA name. Not an offset: "nine" stops meaning nine twice a year. */
  timezone: string
  enabled: boolean
  /** Who set it up. Not who is waiting for the reply -- nobody is. */
  owner_id: string | null
  next_run_at: string | null
  last_run_at: string | null
  last_status: 'ok' | 'failed' | 'skipped' | null
  last_error: string | null
  /** Firings passed over because their time had already gone. */
  skipped: number
  created_at: string
  /** The next few firings, computed on read. */
  upcoming: string[]
  /** Why this will never fire, when that is the case. */
  problem?: string
}

export type ScheduleInput = {
  agent_id: string
  name: string
  prompt: string
  expression: string
  timezone: string
  enabled: boolean
}

export type Preview = {
  upcoming: string[]
  problem?: string
}

export function listSchedules(agentId?: string): Promise<Schedule[]> {
  const q = agentId ? `?agent_id=${encodeURIComponent(agentId)}` : ''
  return api<Schedule[]>(`/v1/schedules${q}`)
}

export function createSchedule(input: ScheduleInput): Promise<Schedule> {
  return api<Schedule>('/v1/schedules', { method: 'POST', body: JSON.stringify(input) })
}

export function updateSchedule(id: string, input: ScheduleInput): Promise<Schedule> {
  return api<Schedule>(`/v1/schedules/${id}`, { method: 'PATCH', body: JSON.stringify(input) })
}

export function deleteSchedule(id: string): Promise<void> {
  return api<void>(`/v1/schedules/${id}`, { method: 'DELETE' })
}

/** What an expression would do, without saving it. */
export function previewSchedule(expression: string, timezone: string): Promise<Preview> {
  return api<Preview>('/v1/schedules/preview', {
    method: 'POST',
    body: JSON.stringify({ expression, timezone }),
  })
}

/**
 * The ordinary shapes, and the expression each produces.
 *
 * Five presets cover nearly everything anybody asks for, and `custom` is the
 * escape hatch rather than the starting point -- a raw expression offered
 * first is a raw expression most people will get wrong.
 */
export type Recurrence = 'daily' | 'weekdays' | 'weekly' | 'monthly' | 'custom'

export function buildExpression(
  kind: Recurrence,
  hour: number,
  minute: number,
  weekdays: number[],
  dayOfMonth: number,
  custom: string,
): string {
  switch (kind) {
    case 'daily':
      return `${minute} ${hour} * * *`
    case 'weekdays':
      return `${minute} ${hour} * * 1-5`
    case 'weekly': {
      // Sunday is 0 in cron, and an empty selection would mean "no days",
      // which parses as nothing and fires never. Monday is the least
      // surprising thing to fall back to.
      const days = weekdays.length > 0 ? [...weekdays].sort().join(',') : '1'
      return `${minute} ${hour} * * ${days}`
    }
    case 'monthly':
      return `${minute} ${hour} ${dayOfMonth} * *`
    case 'custom':
      return custom
  }
}

/**
 * Which preset an existing expression came from, so editing one does not
 * silently move it to Custom.
 *
 * Only the shapes this editor produces are recognised. Anything else is
 * genuinely custom, including an expression somebody wrote by hand that
 * happens to mean the same thing -- rewriting it into a preset would change
 * the stored text under somebody who chose it.
 */
export function readExpression(expression: string): {
  kind: Recurrence
  hour: number
  minute: number
  weekdays: number[]
  dayOfMonth: number
} {
  const fallback = { kind: 'custom' as const, hour: 9, minute: 0, weekdays: [1], dayOfMonth: 1 }
  const parts = expression.trim().split(/\s+/)
  if (parts.length !== 5) return fallback

  const [min, hr, dom, mon, dow] = parts
  const minute = Number(min)
  const hour = Number(hr)
  if (!Number.isInteger(minute) || !Number.isInteger(hour)) return fallback
  if (mon !== '*') return fallback

  if (dom === '*' && dow === '*') return { ...fallback, kind: 'daily', hour, minute }
  if (dom === '*' && dow === '1-5') return { ...fallback, kind: 'weekdays', hour, minute }
  if (dom === '*' && /^[0-6](,[0-6])*$/.test(dow)) {
    return { ...fallback, kind: 'weekly', hour, minute, weekdays: dow.split(',').map(Number) }
  }
  if (dow === '*' && /^\d{1,2}$/.test(dom)) {
    return { ...fallback, kind: 'monthly', hour, minute, dayOfMonth: Number(dom) }
  }
  return fallback
}
