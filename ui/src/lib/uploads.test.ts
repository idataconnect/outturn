import { beforeEach, describe, expect, it, vi } from 'vitest'

vi.mock('./api', async () => {
  const actual = await vi.importActual<typeof import('./api')>('./api')
  return { ...actual, api: vi.fn() }
})

import { api, ApiError } from './api'
import { SINGLE_LIMIT, encodePath, sendFile, type Progress } from './uploads'

const PIECE = 16 * 1024 * 1024
const mocked = vi.mocked(api)

/** A file just over the single-request limit: two pieces. */
function big(): File {
  return new File([new Uint8Array(SINGLE_LIMIT + 10)], 'export.bin')
}

/** Answers the upload calls as the API would, recording the pieces sent. */
function server(opts: { open?: { id: string; received: number[] }; fail?: Record<number, unknown[]> } = {}) {
  const sent: number[] = []
  const fail = opts.fail ?? {}
  mocked.mockImplementation(async (path: string, init?: RequestInit) => {
    const method = init?.method ?? 'GET'
    if (method === 'GET' && opts.open && path.endsWith(`/uploads/${opts.open.id}`)) {
      return { state: 'open', received: opts.open.received, piece_size: PIECE, pieces: 2 }
    }
    if (method === 'POST' && path.endsWith('/uploads')) {
      return { upload_id: 'new', piece_size: PIECE, pieces: 2 }
    }
    const piece = path.match(/\/pieces\/(\d+)$/)
    if (method === 'PUT' && piece) {
      const n = Number(piece[1])
      const failures = fail[n]
      if (failures && failures.length > 0) throw failures.shift()
      sent.push(n)
      return undefined
    }
    if (method === 'POST' && path.endsWith('/complete')) {
      return { path: 'agent/export.bin', scope: 'agent', size: SINGLE_LIMIT + 10 }
    }
    throw new Error(`unexpected ${method} ${path}`)
  })
  return sent
}

describe('sendFile', () => {
  beforeEach(() => {
    mocked.mockReset()
    localStorage.clear()
    vi.useRealTimers()
  })

  it('sends a small file in one request', async () => {
    mocked.mockResolvedValue({ path: 'session/a.txt', scope: 'session', size: 2 })
    await sendFile('s', 'session', 'notes/a b.txt', new File(['hi'], 'a b.txt'), () => {})
    expect(mocked).toHaveBeenCalledTimes(1)
    expect(mocked.mock.calls[0][0]).toBe('/v1/agent-sessions/s/files/session/notes/a%20b.txt')
  })

  it('sends a large one in pieces, reporting each as it arrives', async () => {
    const sent = server()
    const seen: Progress[] = []
    await sendFile('s', 'agent', 'export.bin', big(), (p) => seen.push(p))
    expect(sent.sort()).toEqual([1, 2])
    const last = seen[seen.length - 1]
    expect(last.pieces).toEqual({ done: 2, total: 2 })
    expect(last.sent).toBe(SINGLE_LIMIT + 10)
    // Finished, so nothing is left to resume.
    expect(Object.keys(localStorage)).toEqual([])
  })

  it('carries on an upload this browser began, sending only what is missing', async () => {
    const file = big()
    localStorage.setItem(
      `outturn:upload:s:agent:export.bin:${file.size}:${file.lastModified}`,
      'earlier',
    )
    const sent = server({ open: { id: 'earlier', received: [1] } })
    await sendFile('s', 'agent', 'export.bin', file, () => {})
    expect(sent).toEqual([2])
    expect(mocked.mock.calls.some(([p, i]) => p.endsWith('/uploads') && i?.method === 'POST')).toBe(
      false,
    )
  })

  it('tries a piece again after the server has a moment, and not after a refusal', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true })
    const sent = server({ fail: { 2: [new ApiError(503, 'busy')] } })
    await sendFile('s', 'agent', 'export.bin', big(), () => {})
    expect(sent.sort()).toEqual([1, 2])

    mocked.mockReset()
    server({ fail: { 1: [new ApiError(400, 'piece 1 must be 16777216 bytes')] } })
    await expect(sendFile('s', 'agent', 'other.bin', big(), () => {})).rejects.toThrow('must be')
  })
})

describe('encodePath', () => {
  it('keeps folders as folders', () => {
    expect(encodePath('a b/c#d.txt')).toBe('a%20b/c%23d.txt')
  })
})
