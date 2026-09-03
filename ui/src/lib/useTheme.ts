import { useCallback, useEffect, useState } from 'react'

import { applyTheme, readTheme, storeTheme, type Theme } from './theme'

export function useTheme(): [Theme, (theme: Theme) => void] {
  // Read during initialisation rather than in an effect, so the first render
  // already matches what the inline script in index.html applied.
  const [theme, setThemeState] = useState<Theme>(readTheme)

  const setTheme = useCallback((next: Theme) => {
    setThemeState(next)
    storeTheme(next)
    applyTheme(next)
  }, [])

  // While following the system, keep following it: a laptop switching to dark
  // at sunset should carry the page with it without a reload.
  useEffect(() => {
    if (theme !== 'system') return

    const query = window.matchMedia('(prefers-color-scheme: dark)')
    const onChange = () => applyTheme('system')
    query.addEventListener('change', onChange)
    return () => query.removeEventListener('change', onChange)
  }, [theme])

  return [theme, setTheme]
}
