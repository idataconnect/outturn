import { useEffect, useState } from 'react'

/**
 * How much room there is, in the three sizes the layout actually distinguishes.
 *
 * - `phone`   below 768px: one thing at a time; panels are overlays.
 * - `tablet`  768-1279px: the thread plus at most one panel beside it.
 * - `desktop` 1280px and up: room for both panels at once.
 *
 * The old layout turned everything on at 1024px, which is where three fixed
 * panels totalling 768px leave 256px of conversation. `desktop` therefore
 * starts at 1280, the first width where showing both is not a cruelty.
 */
export type Breakpoint = 'phone' | 'tablet' | 'desktop'

const QUERIES: [Breakpoint, string][] = [
  ['desktop', '(min-width: 1280px)'],
  ['tablet', '(min-width: 768px)'],
]

export function currentBreakpoint(): Breakpoint {
  // Server-rendered or pre-hydration there is no window; assume the roomy
  // case, which matches what the CSS shows before JavaScript runs.
  if (typeof window === 'undefined' || !window.matchMedia) return 'desktop'
  for (const [name, query] of QUERIES) {
    if (window.matchMedia(query).matches) return name
  }
  return 'phone'
}

export function useBreakpoint(): Breakpoint {
  // Read during initialisation, so the first render is already correct rather
  // than flashing the wrong layout and correcting it.
  const [breakpoint, setBreakpoint] = useState<Breakpoint>(currentBreakpoint)

  useEffect(() => {
    if (!window.matchMedia) return
    const lists = QUERIES.map(([, query]) => window.matchMedia(query))
    const onChange = () => setBreakpoint(currentBreakpoint())
    for (const list of lists) list.addEventListener('change', onChange)
    // A rotation between mount and now would otherwise go unnoticed.
    onChange()
    return () => {
      for (const list of lists) list.removeEventListener('change', onChange)
    }
  }, [])

  return breakpoint
}
