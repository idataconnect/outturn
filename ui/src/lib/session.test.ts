import { describe, expect, it, vi } from 'vitest'

import { ApiError, NetworkError } from './api'
import { restoreSession, retryDelay } from './session'

describe('restoring a session', () => {
  it('signs out at once when the API says the session is over', async () => {
    const reconnecting = vi.fn()
    const read = vi.fn().mockRejectedValue(new ApiError(401, 'not authenticated'))
    expect(await restoreSession(read, reconnecting, async () => {})).toBeNull()
    expect(read).toHaveBeenCalledTimes(1)
    expect(reconnecting).not.toHaveBeenCalled()
  })

  it('waits out an API that is down for however long it takes', async () => {
    const reconnecting = vi.fn()
    const waited: number[] = []
    const read = vi.fn()
    // Down well past where it used to give up: unreachable, then a proxy's
    // 502, then a 503 while it starts.
    for (let i = 0; i < 8; i++) read.mockRejectedValueOnce(new NetworkError('unreachable'))
    read.mockRejectedValueOnce(new ApiError(502, 'Bad Gateway'))
    read.mockRejectedValueOnce(new ApiError(503, 'starting'))
    read.mockResolvedValueOnce({ session_id: 's' })
    const session = await restoreSession(read, reconnecting, async (ms) => {
      waited.push(ms)
    })
    expect(session).toEqual({ session_id: 's' })
    expect(reconnecting).toHaveBeenCalledTimes(10)
    expect(waited.slice(0, 4)).toEqual([1000, 2000, 4000, 8000])
    expect(Math.max(...waited)).toBe(10_000)
  })

  it('backs off to every ten seconds', () => {
    expect(retryDelay(0)).toBe(1000)
    expect(retryDelay(3)).toBe(8000)
    expect(retryDelay(50)).toBe(10_000)
  })
})

describe('an answer that is not an outage', () => {
  it('is said rather than retried, since it will be the same next time', async () => {
    const reconnecting = vi.fn()
    const read = vi.fn().mockRejectedValue(new ApiError(403, 'not allowed here'))
    await expect(restoreSession(read, reconnecting, async () => {})).rejects.toThrow('not allowed here')
    expect(read).toHaveBeenCalledTimes(1)
    expect(reconnecting).not.toHaveBeenCalled()
  })
})
