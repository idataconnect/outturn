import { act, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { ApiError, NetworkError } from './api'
import { loadHistory, pollEvents } from './chat'
import { useChatRuntime } from './useChatRuntime'

// What the hook handed the runtime last: the messages the thread would draw.
let drawn: Array<{ metadata: { parts?: Array<{ type: string; text?: string }> } }> = []
vi.mock('@assistant-ui/react', () => ({
  useExternalStoreRuntime: (config: { messages: typeof drawn }) => {
    drawn = config.messages
    return {}
  },
}))

vi.mock('./chat', () => ({
  sendMessage: vi.fn(),
  loadHistory: vi.fn(),
  pollEvents: vi.fn(() => new Promise(() => {})),
  cancelTurn: vi.fn(),
  retryTurn: vi.fn(),
}))

const history = { messages: [], cursor: 'c1' }

describe('a history load that fails', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.useFakeTimers()
  })
  afterEach(() => {
    vi.useRealTimers()
  })

  it('says so, asks again, and follows the feed once it loads', async () => {
    vi.mocked(loadHistory)
      .mockRejectedValueOnce(new NetworkError('Could not reach the API.'))
      .mockResolvedValue(history as never)
    const { result } = renderHook(() => useChatRuntime('s1'))

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
    })
    expect(result.current.error).toBe('Could not reach the API.')
    expect(pollEvents).not.toHaveBeenCalled()

    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000)
    })
    expect(loadHistory).toHaveBeenCalledTimes(2)
    expect(result.current.error).toBeNull()
    expect(pollEvents).toHaveBeenCalledWith('s1', 'c1', expect.anything())
  })

  it('is not asked again when the session is over', async () => {
    vi.mocked(loadHistory).mockRejectedValue(new ApiError(401, 'no'))
    const { result } = renderHook(() => useChatRuntime('s1'))

    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000)
    })
    expect(loadHistory).toHaveBeenCalledTimes(1)
    expect(result.current.error).toBeNull()
    expect(pollEvents).not.toHaveBeenCalled()
  })
})

describe('thinking heard twice', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('is written once', async () => {
    const reply = {
      id: 'a1',
      role: 'assistant',
      content: '',
      metadata: {},
      delta_next: 0,
      model: null,
      replies_to: null,
      absorbed_by: null,
      job_state: null,
    }
    vi.mocked(loadHistory).mockResolvedValue({ messages: [reply], cursor: 'c1' } as never)
    const batch = {
      cursor: 'c2',
      events: [{ id: 'e1', kind: 'chat.reasoning', payload: { message_id: 'a1', text: 'hm' } }],
    }
    // The same batch twice, as a replay would deliver it, then silence.
    vi.mocked(pollEvents)
      .mockResolvedValueOnce(batch as never)
      .mockResolvedValueOnce(batch as never)
      .mockImplementation(() => new Promise(() => {}))
    renderHook(() => useChatRuntime('s1'))

    await vi.waitFor(() => expect(pollEvents).toHaveBeenCalledTimes(3))
    await act(async () => {})
    expect(drawn[0].metadata.parts).toEqual([expect.objectContaining({ type: 'reasoning', text: 'hm' })])
  })
})
