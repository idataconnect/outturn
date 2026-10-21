import { act, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { ApiError, NetworkError } from './api'
import { loadHistory, pollEvents } from './chat'
import { useChatRuntime } from './useChatRuntime'

vi.mock('@assistant-ui/react', () => ({
  useExternalStoreRuntime: () => ({}),
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
