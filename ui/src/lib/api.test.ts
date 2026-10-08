import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { ApiError, api, onSessionEnded } from './api'

/** Answers each fetch from `script` in order, by path. */
function serve(script: Array<[string, number]>) {
  const calls: string[] = []
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: string) => {
      calls.push(path)
      const next = script.shift()
      if (!next || next[0] !== path) throw new Error(`unexpected fetch ${path}`)
      const [, status] = next
      return new Response(status === 204 ? null : '{}', {
        status,
        headers: { 'content-type': 'application/json' },
      })
    }),
  )
  return calls
}

describe('a 401', () => {
  let ended: ReturnType<typeof vi.fn<() => void>>
  let stop: () => void

  beforeEach(() => {
    ended = vi.fn<() => void>()
    stop = onSessionEnded(ended)
  })
  afterEach(() => {
    stop()
    vi.unstubAllGlobals()
  })

  it('refreshes once and carries on when the refresh works', async () => {
    serve([
      ['/v1/agents', 401],
      ['/v1/session/refresh', 204],
      ['/v1/agents', 200],
    ])
    await expect(api('/v1/agents')).resolves.toEqual({})
    expect(ended).not.toHaveBeenCalled()
  })

  it('ends the session when the refresh is refused', async () => {
    serve([
      ['/v1/agents', 401],
      ['/v1/session/refresh', 401],
    ])
    await expect(api('/v1/agents')).rejects.toBeInstanceOf(ApiError)
    expect(ended).toHaveBeenCalledOnce()
  })

  it('ends the session when a fresh token is refused too', async () => {
    serve([
      ['/v1/agents', 401],
      ['/v1/session/refresh', 204],
      ['/v1/agents', 401],
    ])
    await expect(api('/v1/agents')).rejects.toBeInstanceOf(ApiError)
    expect(ended).toHaveBeenCalledOnce()
  })

  it('from signing in is a wrong password, not an ended session', async () => {
    const calls = serve([['/v1/login', 401]])
    await expect(api('/v1/login', { method: 'POST' })).rejects.toBeInstanceOf(ApiError)
    expect(calls).toEqual(['/v1/login'])
    expect(ended).not.toHaveBeenCalled()
  })

  it('does not end the session when the API is only unreachable', async () => {
    serve([
      ['/v1/agents', 401],
      ['/v1/session/refresh', 503],
    ])
    await expect(api('/v1/agents')).rejects.toBeInstanceOf(ApiError)
    expect(ended).not.toHaveBeenCalled()
  })
})
