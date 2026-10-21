import { afterEach, describe, expect, it, vi } from 'vitest'

import { mergeRecent, pollEvents, type AgentSession } from './chat'

const session = (id: string, at: string, turn: AgentSession['turn'] = null): AgentSession => ({
  id,
  agent_id: 'a',
  title: id,
  last_active_at: at,
  turn,
})

describe('mergeRecent', () => {
  it('lets the fresh page replace what it covers and keeps everything older', () => {
    const held = [
      session('busy', '2026-10-03T10:00:00Z', 'running'),
      session('old', '2026-09-01T00:00:00Z'),
    ]
    // The turn finished: the session was touched and its turn is gone.
    const fresh = [session('busy', '2026-10-03T10:05:00Z')]

    const merged = mergeRecent(held, fresh)
    expect(merged.map((s) => s.id)).toEqual(['busy', 'old'])
    expect(merged[0].turn).toBeNull()
  })

  it('moves an old conversation picked up again to the top', () => {
    const held = [session('b', '2026-10-02T00:00:00Z'), session('a', '2026-09-01T00:00:00Z')]
    const fresh = [session('a', '2026-10-03T00:00:00Z', 'pending'), session('b', '2026-10-02T00:00:00Z')]

    expect(mergeRecent(held, fresh).map((s) => s.id)).toEqual(['a', 'b'])
  })
})

describe('pollEvents', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('encodes the session and the cursor into the query', async () => {
    const fetched = vi.fn(
      async () =>
        new Response('{"events":[],"cursor":""}', {
          headers: { 'content-type': 'application/json' },
        }),
    )
    vi.stubGlobal('fetch', fetched)
    await pollEvents('a&b=c', 'x y')
    expect(fetched.mock.calls[0]).toContain('/v1/events?session_id=a%26b%3Dc&after=x%20y')
  })
})
