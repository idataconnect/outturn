import { useEffect, useState } from 'react'

const QUERY = '(prefers-reduced-motion: reduce)'

/**
 * Whether this reader has asked for less movement.
 *
 * CSS can answer this on its own, and where a media query will do it should:
 * this exists for animation CSS cannot reach -- SMIL inside an SVG, which
 * `prefers-reduced-motion` does not apply to, so the only way to stop it is
 * not to render it.
 *
 * Followed rather than read once. Somebody turning the setting on is asking
 * for the movement to stop now, not after a reload.
 */
export function useReducedMotion(): boolean {
  const [reduced, setReduced] = useState(() => {
    // Read during initialisation so the first render already agrees with the
    // setting, rather than animating for a frame and then stopping.
    if (typeof window === 'undefined' || !window.matchMedia) return false
    return window.matchMedia(QUERY).matches
  })

  useEffect(() => {
    if (!window.matchMedia) return
    const query = window.matchMedia(QUERY)
    const onChange = () => setReduced(query.matches)
    query.addEventListener('change', onChange)
    return () => query.removeEventListener('change', onChange)
  }, [])

  return reduced
}
