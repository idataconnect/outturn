import { useCallback, useEffect, useRef, useState } from 'react'

import { elapsedSince } from './elapsed'

/** How long the figure stays up before it is withdrawn as stale. */
const VISIBLE_MS = 20_000

/**
 * How long ago a message was written, worked out when the pointer arrives.
 *
 * Nothing is stored, cached or computed until somebody asks: a relative time is
 * wrong the moment after it is derived, so the only safe place to derive one is
 * the point of display. What persists is the message id, whose first 48 bits
 * are the instant it was minted -- so the figure comes from the transcript's own
 * ordering rather than from a second copy of the time that could disagree.
 *
 * It does not tick. A figure counting upwards would be precise about something
 * nobody needs precisely, and anything under a minute reads "Just now", which is
 * both what a person means and the honest resolution of a number computed once.
 * It withdraws itself after a while, so a pointer left resting over a message
 * does not leave a figure sitting there slowly becoming a lie.
 *
 * Returns the phrase, whether it should be shown, and the handlers to spread
 * onto the bubble. The thing you point at and the thing that appears cannot be
 * one element, so the bubble owns the pointer and the label owns the space.
 *
 * `phrase` and `shown` are deliberately two values rather than one nullable
 * one. A transition needs a painted starting state, and if the text arrived in
 * the same commit that turned the opacity up there would be nothing to animate
 * from -- which is exactly how this first went in, fading not at all. So the
 * phrase mounts first and the reveal follows on the next frame.
 */
export function useMessageAge(id: string) {
  const [phrase, setPhrase] = useState<string | null>(null)
  const [shown, setShown] = useState(false)
  const hideTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const frame = useRef<number | null>(null)

  const clear = useCallback(() => {
    if (hideTimer.current !== null) {
      clearTimeout(hideTimer.current)
      hideTimer.current = null
    }
    if (frame.current !== null) {
      cancelAnimationFrame(frame.current)
      frame.current = null
    }
  }, [])

  // A pending timer outlives the component if the thread re-renders it away
  // mid-hover, and firing into something unmounted is a warning at best.
  useEffect(() => clear, [clear])

  const show = useCallback(() => {
    // Computed here, at the moment of asking. That is the whole design: no
    // state holds an age, only the id one can be derived from.
    const said = elapsedSince(id)
    if (said === null) return
    clear()
    setPhrase(said)
    // Next frame, so the text is painted at zero opacity before it is turned
    // up. Same commit and the browser has no previous value to transition
    // from, and the figure simply appears.
    frame.current = requestAnimationFrame(() => {
      frame.current = null
      setShown(true)
    })
    hideTimer.current = setTimeout(() => setShown(false), VISIBLE_MS)
  }, [id, clear])

  const hide = useCallback(() => {
    clear()
    // The phrase stays mounted while it fades out; only the opacity changes,
    // so there is something on screen for the transition to act on.
    setShown(false)
  }, [clear])

  return {
    phrase,
    shown,
    /** Spread onto the bubble: it owns the pointer, and focus for anyone without one. */
    handlers: {
      onPointerEnter: show,
      onPointerLeave: hide,
      onFocus: show,
      onBlur: hide,
    },
  }
}
