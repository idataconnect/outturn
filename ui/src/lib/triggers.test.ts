import { describe, expect, it } from 'vitest'

import type { Schedule } from './schedules'
import type { Webhook } from './webhooks'
import { fromSchedule, fromWebhook, ordered, startedByPhrase, triggerOf } from './triggers'

const schedule = (over: Partial<Schedule>): Schedule => ({
  id: 's',
  workspace_id: 'w',
  agent_id: 'a',
  name: 'Morning summary',
  prompt: 'p',
  expression: '0 9 * * *',
  timezone: 'UTC',
  enabled: true,
  owner_id: null,
  next_run_at: null,
  last_run_at: null,
  last_status: null,
  last_error: null,
  skipped: 0,
  created_at: '2026-10-01T00:00:00Z',
  upcoming: ['2026-10-09T09:00:00Z'],
  ...over,
})

const webhook = (over: Partial<Webhook>): Webhook => ({
  id: 'h',
  agent_id: 'a',
  name: 'New reservations',
  scheme: 'hmac',
  prompt: '{{body}}',
  enabled: true,
  account: null,
  max_per_hour: 60,
  last_at: null,
  last_status: null,
  last_error: null,
  refused: 0,
  created_at: '2026-10-01T00:00:00Z',
  endpoint: '/v1/hooks/x',
  ...over,
})

describe('what the panel says', () => {
  it('a schedule that cannot fire is in trouble, for the reason it gives', () => {
    const t = fromSchedule(schedule({ problem: 'February has no 30th', upcoming: [] }))
    expect(t.trouble).toBe('February has no 30th')
  })

  it('a schedule whose last run failed is in trouble', () => {
    const t = fromSchedule(schedule({ last_status: 'failed', last_error: 'the model refused' }))
    expect(t.trouble).toBe('the model refused')
  })

  it('a schedule that is off says so rather than when it would run', () => {
    expect(fromSchedule(schedule({ enabled: false })).status).toBe('Off')
  })

  it('a webhook refused for its rate says why', () => {
    const t = fromWebhook(webhook({ last_status: 'refused', last_error: 'rate limited' }))
    expect(t.trouble).toBe('The last request was refused: rate limited')
  })

  it('a webhook nobody has called yet says that, and is not in trouble', () => {
    const t = fromWebhook(webhook({}))
    expect(t.trouble).toBeNull()
    expect(t.status).toBe('No requests yet')
  })
})

describe('ordered', () => {
  it('puts trouble first, then what is on by its next run, then what is off', () => {
    const list = ordered([
      fromSchedule(schedule({ id: 'off', name: 'Off', enabled: false })),
      fromWebhook(webhook({ id: 'hook', name: 'Hook' })),
      fromSchedule(schedule({ id: 'later', name: 'Later', upcoming: ['2026-10-10T09:00:00Z'] })),
      fromSchedule(schedule({ id: 'sooner', name: 'Sooner', upcoming: ['2026-10-09T09:00:00Z'] })),
      fromWebhook(webhook({ id: 'broken', name: 'Broken', last_status: 'failed' })),
    ])
    expect(list.map((t) => t.id)).toEqual(['broken', 'sooner', 'later', 'hook', 'off'])
  })
})

describe('what started a conversation', () => {
  it('names the trigger while it exists', () => {
    expect(startedByPhrase({ kind: 'schedule', name: 'Morning arrivals' })).toBe(
      'the Morning arrivals schedule',
    )
  })

  it('says what it was once it is gone', () => {
    expect(startedByPhrase({ kind: 'webhook', name: null })).toBe('a webhook since deleted')
  })

  it('reads the trigger an opening message recorded', () => {
    expect(triggerOf({ schedule_id: 's', schedule_name: 'Morning arrivals' })).toEqual({
      kind: 'schedule',
      name: 'Morning arrivals',
    })
    expect(triggerOf({ webhook_trigger_id: 'h', webhook_trigger_name: 'Weather alerts' })).toEqual({
      kind: 'webhook',
      name: 'Weather alerts',
    })
  })

  it('finds nothing on a message a person sent', () => {
    expect(triggerOf({})).toBeNull()
    expect(triggerOf({ quoted: 'something' })).toBeNull()
  })
})
