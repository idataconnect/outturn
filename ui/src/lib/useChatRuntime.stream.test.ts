import { act, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { ApiError, NetworkError } from './api'
import { loadHistory, pollEvents, type Message } from './chat'
import { useChatRuntime } from './useChatRuntime'

// What the hook handed the runtime last: the messages the thread would draw.
let drawn: Array<{ id: string; content: string }> = []
vi.mock('@assistant-ui/react', () => ({
  useExternalStoreRuntime: (config: { messages: typeof drawn }) => {
    drawn = config.messages
    return {}
  },
}))

vi.mock('./chat', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./chat')>()),
  sendMessage: vi.fn(),
  loadHistory: vi.fn(),
  pollEvents: vi.fn(() => new Promise(() => {})),
  cancelTurn: vi.fn(),
  retryTurn: vi.fn(),
}))

const msg = (m: Partial<Message> & Pick<Message, 'id' | 'role'>): Message => ({
  content: '',
  metadata: {},
  delta_next: 0,
  model: null,
  replies_to: null,
  absorbed_by: null,
  job_state: null,
  ...m,
})

const prompt = msg({ id: '01a0-u1', role: 'user', content: 'Hi', job_state: 'running' })
const reply = msg({ id: '01a0-a1', role: 'assistant', replies_to: '01a0-u1' })
const delta = (id: string, idx: number, text: string) => ({
  id,
  kind: 'chat.delta',
  payload: { message_id: reply.id, idx, text },
})
const never = () => new Promise<never>(() => {})
/** The reply as the thread would draw it. */
const replyText = () => drawn.find((m) => m.id === reply.id)?.content

/** A promise and the functions that settle it, for a request held open. */
function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

beforeEach(() => {
  vi.clearAllMocks()
  drawn = []
})

describe('the feed, after a fetch that failed', () => {
  beforeEach(() => {
    vi.useFakeTimers()
  })
  afterEach(() => {
    vi.useRealTimers()
  })

  it('asks again from the cursor it had, and writes what it hears once', async () => {
    vi.mocked(loadHistory).mockResolvedValue({ messages: [prompt, reply], cursor: 'c1' } as never)
    vi.mocked(pollEvents)
      .mockRejectedValueOnce(new NetworkError('Could not reach the API.'))
      .mockResolvedValueOnce({ cursor: 'c2', events: [delta('e1', 0, 'Hello')] } as never)
      .mockImplementation(never)
    const { result } = renderHook(() => useChatRuntime('s1'))

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
    })
    expect(pollEvents).toHaveBeenCalledTimes(1)
    // Backed off rather than spinning.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1999)
    })
    expect(pollEvents).toHaveBeenCalledTimes(1)

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1)
    })
    expect(vi.mocked(pollEvents).mock.calls.map((c) => c[1])).toEqual(['c1', 'c1', 'c2'])
    expect(replyText()).toBe('Hello')
    // A dropped poll is the ordinary weather of a long-lived connection, and
    // the reconnect is the answer to it; nothing is wrong to report.
    expect(result.current.error).toBeNull()
  })
})

describe('a batch heard again', () => {
  it('adds only what it had not already said, in order', async () => {
    vi.mocked(loadHistory).mockResolvedValue({ messages: [prompt, reply], cursor: 'c1' } as never)
    const first = { cursor: 'c2', events: [delta('e1', 0, 'One, '), delta('e2', 1, 'two, ')] }
    // The second batch overlaps the first, as a replay from an older cursor
    // does, and carries one fragment more.
    const replay = {
      cursor: 'c3',
      events: [delta('e1', 0, 'One, '), delta('e2', 1, 'two, '), delta('e3', 2, 'three.')],
    }
    vi.mocked(pollEvents)
      .mockResolvedValueOnce(first as never)
      .mockResolvedValueOnce(first as never)
      .mockResolvedValueOnce(replay as never)
      .mockImplementation(never)
    renderHook(() => useChatRuntime('s1'))

    await vi.waitFor(() => expect(pollEvents).toHaveBeenCalledTimes(4))
    await act(async () => {})
    expect(replyText()).toBe('One, two, three.')
    expect(loadHistory).toHaveBeenCalledTimes(1)
  })

  it('does not count fragments history already holds', async () => {
    // Loaded with the first fragment stored, then sent it again by the feed.
    const partly = { ...reply, content: 'One, ', delta_next: 1 }
    vi.mocked(loadHistory).mockResolvedValue({ messages: [prompt, partly], cursor: 'c1' } as never)
    vi.mocked(pollEvents)
      .mockResolvedValueOnce({
        cursor: 'c2',
        events: [delta('e1', 0, 'One, '), delta('e2', 1, 'two.')],
      } as never)
      .mockImplementation(never)
    renderHook(() => useChatRuntime('s1'))

    await vi.waitFor(() => expect(pollEvents).toHaveBeenCalledTimes(2))
    await act(async () => {})
    expect(replyText()).toBe('One, two.')
  })
})

describe('switching conversation mid-stream', () => {
  it('lets nothing the old feed hears land in the new one', async () => {
    const other = msg({ id: '01b0-u1', role: 'user', content: 'Elsewhere' })
    vi.mocked(loadHistory).mockImplementation(async (session: string) =>
      (session === 's1'
        ? { messages: [prompt, reply], cursor: 'c1' }
        : { messages: [other], cursor: 'd1' }) as never,
    )
    const oldPoll = deferred<unknown>()
    let oldSignal: AbortSignal | undefined
    vi.mocked(pollEvents).mockImplementation((session: string, _cursor, signal) => {
      if (session === 's1') {
        oldSignal = signal
        return oldPoll.promise as never
      }
      return never()
    })
    const { rerender } = renderHook(({ id }) => useChatRuntime(id), {
      initialProps: { id: 's1' },
    })
    await vi.waitFor(() => expect(pollEvents).toHaveBeenCalledWith('s1', 'c1', expect.anything()))

    rerender({ id: 's2' })
    await vi.waitFor(() => expect(pollEvents).toHaveBeenCalledWith('s2', 'd1', expect.anything()))
    expect(oldSignal?.aborted).toBe(true)

    // The old poll answers anyway, as one already on the wire can: a fragment
    // of the old reply and a whole new message of the old conversation.
    await act(async () => {
      oldPoll.resolve({
        cursor: 'c2',
        events: [
          delta('e1', 0, 'Late'),
          {
            id: 'e2',
            kind: 'chat.message',
            payload: msg({ id: '01a0-u2', role: 'user', content: 'Late too' }),
          },
        ],
      })
    })
    expect(drawn.map((m) => m.id)).toEqual([other.id])
  })

  it('keeps the new conversation when the old one\'s history arrives late', async () => {
    const other = msg({ id: '01b0-u1', role: 'user', content: 'Elsewhere' })
    const oldHistory = deferred<unknown>()
    vi.mocked(loadHistory).mockImplementation((session: string) =>
      (session === 's1'
        ? oldHistory.promise
        : Promise.resolve({ messages: [other], cursor: 'd1' })) as never,
    )
    vi.mocked(pollEvents).mockImplementation(never)
    const { rerender } = renderHook(({ id }) => useChatRuntime(id), {
      initialProps: { id: 's1' },
    })
    rerender({ id: 's2' })
    await vi.waitFor(() => expect(pollEvents).toHaveBeenCalledWith('s2', 'd1', expect.anything()))

    await act(async () => {
      oldHistory.resolve({ messages: [prompt, reply], cursor: 'c1' })
    })
    expect(drawn.map((m) => m.id)).toEqual([other.id])
    expect(pollEvents).not.toHaveBeenCalledWith('s1', expect.anything(), expect.anything())
  })
})

describe('a history that cannot be loaded', () => {
  beforeEach(() => {
    vi.useFakeTimers()
  })
  afterEach(() => {
    vi.useRealTimers()
  })

  it('says why when the API refuses it, rather than drawing an empty thread', async () => {
    vi.mocked(loadHistory).mockRejectedValue(new ApiError(500, 'the transcript could not be read'))
    const { result } = renderHook(() => useChatRuntime('s1'))

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
    })
    expect(result.current.error).toBe('the transcript could not be read')
    expect(pollEvents).not.toHaveBeenCalled()
    // Still trying, and still saying so while it does.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000)
    })
    expect(loadHistory).toHaveBeenCalledTimes(2)
    expect(result.current.error).toBe('the transcript could not be read')
  })

  // The hook keeps `messages` across a change of `sessionId` and replaces
  // them only when the new history arrives, so until then -- and for as long
  // as it keeps failing -- the last conversation is drawn under the new one.
  // Skipped for that bug; unskip with the fix.
  it('does not go on drawing the conversation that was open before', async () => {
    vi.mocked(loadHistory).mockImplementation(async (session: string) => {
      if (session === 's1') return { messages: [prompt, reply], cursor: 'c1' } as never
      throw new NetworkError('Could not reach the API.')
    })
    vi.mocked(pollEvents).mockImplementation(never)
    const { result, rerender } = renderHook(({ id }) => useChatRuntime(id), {
      initialProps: { id: 's1' },
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
    })
    expect(drawn.map((m) => m.id)).toEqual([prompt.id, reply.id])

    rerender({ id: 's2' })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
    })
    expect(result.current.error).toBe('Could not reach the API.')
    expect(drawn).toEqual([])
  })
})
