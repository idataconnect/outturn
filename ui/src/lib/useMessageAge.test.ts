import { act, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { useMessageAge } from './useMessageAge'

/** A v7 id minted at a given instant, so a test can name the time it means. */
function idAt(millis: number): string {
  const hex = millis.toString(16).padStart(12, '0')
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-7abc-8def-0123456789ab`
}

const NOW = Date.UTC(2026, 8, 19, 12, 0, 0)

describe('revealing how old a message is', () => {
  beforeEach(() => {
    // Both clocks are faked, and neither test waits for anything: the 20s
    // withdrawal is advanced rather than slept through, so this suite costs
    // milliseconds however long the real timeout is.
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'Date', 'requestAnimationFrame', 'cancelAnimationFrame'] })
    vi.setSystemTime(NOW)
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it('says nothing until asked', () => {
    // The whole design: no age exists until somebody points at the message.
    const { result } = renderHook(() => useMessageAge(idAt(NOW - 3600_000)))
    expect(result.current.phrase).toBeNull()
    expect(result.current.shown).toBe(false)
  })

  it('works the age out when the pointer arrives', () => {
    const { result } = renderHook(() => useMessageAge(idAt(NOW - 3600_000)))
    act(() => result.current.handlers.onPointerEnter())
    expect(result.current.phrase).toBe('1 hour ago')
  })

  it('mounts the text before it turns the opacity up', () => {
    // A transition needs a painted starting state. If `shown` went true in the
    // same commit the text arrived, there would be nothing to fade from --
    // which is exactly how this first went in, fading not at all.
    const { result } = renderHook(() => useMessageAge(idAt(NOW - 3600_000)))
    act(() => result.current.handlers.onPointerEnter())
    expect(result.current.phrase).toBe('1 hour ago')
    expect(result.current.shown).toBe(false)

    act(() => {
      vi.advanceTimersToNextFrame()
    })
    expect(result.current.shown).toBe(true)
  })

  it('withdraws the figure once it has been up a while', () => {
    const { result } = renderHook(() => useMessageAge(idAt(NOW - 3600_000)))
    act(() => result.current.handlers.onPointerEnter())
    act(() => {
      vi.advanceTimersToNextFrame()
    })
    expect(result.current.shown).toBe(true)

    // A figure computed once and left on screen becomes a lie by degrees, so
    // it takes itself down rather than ageing in place.
    act(() => {
      vi.advanceTimersByTime(20_000)
    })
    expect(result.current.shown).toBe(false)
  })

  it('keeps the text mounted while it fades out', () => {
    // Only the opacity changes on the way out; unmounting the text would leave
    // the transition nothing to act on.
    const { result } = renderHook(() => useMessageAge(idAt(NOW - 3600_000)))
    act(() => result.current.handlers.onPointerEnter())
    act(() => {
      vi.advanceTimersToNextFrame()
    })
    act(() => result.current.handlers.onPointerLeave())
    expect(result.current.shown).toBe(false)
    expect(result.current.phrase).toBe('1 hour ago')
  })

  it('recomputes on a later hover rather than reusing the figure', () => {
    // The point of not caching: an hour later, the same message is older.
    const id = idAt(NOW - 3600_000)
    const { result } = renderHook(() => useMessageAge(id))
    act(() => result.current.handlers.onPointerEnter())
    expect(result.current.phrase).toBe('1 hour ago')

    act(() => result.current.handlers.onPointerLeave())
    vi.setSystemTime(NOW + 7200_000)
    act(() => result.current.handlers.onPointerEnter())
    expect(result.current.phrase).toBe('3 hours ago')
  })

  it('restarts the countdown when the pointer returns', () => {
    const { result } = renderHook(() => useMessageAge(idAt(NOW - 3600_000)))
    act(() => result.current.handlers.onPointerEnter())
    act(() => {
      vi.advanceTimersByTime(15_000)
    })
    // Back again before it withdrew: the figure is fresh, so its time on
    // screen should be too.
    act(() => result.current.handlers.onPointerEnter())
    act(() => {
      vi.advanceTimersToNextFrame()
      vi.advanceTimersByTime(15_000)
    })
    expect(result.current.shown).toBe(true)
  })

  it('says nothing for an id it cannot read', () => {
    // A v4 id has no timestamp in it, and inventing one would date the message
    // to somewhere in the wrong century.
    const { result } = renderHook(() => useMessageAge('9f1c2d3e-4b5a-4c6d-8e9f-0123456789ab'))
    act(() => result.current.handlers.onPointerEnter())
    expect(result.current.phrase).toBeNull()
    expect(result.current.shown).toBe(false)
  })

  it('drops its timers when the message goes away mid-hover', () => {
    // The thread re-renders messages away while a pointer is over one, and a
    // timer firing into an unmounted component is a warning at best.
    const { result, unmount } = renderHook(() => useMessageAge(idAt(NOW - 3600_000)))
    act(() => result.current.handlers.onPointerEnter())
    unmount()
    expect(() => {
      vi.advanceTimersByTime(60_000)
    }).not.toThrow()
  })
})
