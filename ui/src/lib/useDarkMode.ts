import { useEffect, useState } from 'react'

/**
 * Whether the page is currently dark, as a value a component can render from.
 *
 * `useTheme` returns the *choice* -- light, dark or system -- which is the
 * right thing for a control that sets it and the wrong thing for a chart: SVG
 * fills are attributes rather than CSS, so a chart has to resolve the choice to
 * a colour itself, and under "system" the answer changes without anything in
 * React re-rendering.
 *
 * So this watches the `dark` class on <html> rather than the media query or the
 * stored choice. That class is what `applyTheme` sets and what every stylesheet
 * keys off, which makes it the one place where the three inputs -- the stored
 * choice, the operating system, and the inline script that runs before React --
 * have already been resolved into a single answer. Watching anything else
 * reintroduces a way for the charts to disagree with the page around them.
 */
export function useDarkMode(): boolean {
  const [dark, setDark] = useState(() => document.documentElement.classList.contains('dark'))

  useEffect(() => {
    const target = document.documentElement
    const observer = new MutationObserver(() => {
      setDark(target.classList.contains('dark'))
    })
    observer.observe(target, { attributes: true, attributeFilter: ['class'] })
    // The class may have changed between the initial read and this effect.
    setDark(target.classList.contains('dark'))
    return () => observer.disconnect()
  }, [])

  return dark
}
