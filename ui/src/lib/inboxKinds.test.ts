import { describe, expect, it } from 'vitest'

import type { ActionItem } from './actions'
import { describeItem } from './inboxKinds'

function item(kind: string, payload: Record<string, unknown> = {}): ActionItem {
  return {
    id: '01a0f3a6-2746-7fe3-8665-b385487c76a4',
    workspace_id: 'w',
    kind,
    event_id: null,
    payload,
    state: 'pending',
    created_at: '2026-09-30T00:00:00Z',
    expires_at: null,
  }
}

describe('how an inbox item reads', () => {
  it('names an approval by the act it asks about', () => {
    const d = describeItem(item('approval.budget_override', { reason: 'Over by £40', session_id: 's1' }))
    expect(d.kind).toBe('approval')
    expect(d.label).toBe('Budget override to approve')
    expect(d.summary).toBe('Over by £40')
    expect(d.sessionId).toBe('s1')
  })

  it('says an agent is asleep, and why', () => {
    const d = describeItem(item('sleep', { reason: 'Waiting for the deposit', session_id: 's2' }))
    expect(d.kind).toBe('sleep')
    expect(d.label).toBe('Agent asleep')
    expect(d.summary).toBe('Waiting for the deposit')
  })

  it('still shows a kind nobody has described, rather than a blank row', () => {
    const d = describeItem(item('review_needed', { question: 'Is this right?' }))
    expect(d.kind).toBe('other')
    expect(d.label).toBe('Review needed')
    expect(d.summary).toBe('Is this right?')
  })

  it('reads nothing into a field of the wrong type', () => {
    const d = describeItem(item('sleep', { reason: { nested: true }, session_id: 7 }))
    expect(d.summary).toBeNull()
    expect(d.sessionId).toBeNull()
  })
})
