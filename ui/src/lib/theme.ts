/**
 * Theme selection.
 *
 * Three states rather than two: "system" is a distinct choice from picking
 * light or dark, and must keep following the operating system when it changes
 * rather than freezing whatever it was at load.
 */

export type Theme = 'light' | 'dark' | 'system'

const STORAGE_KEY = 'outturn.theme'

export function readTheme(): Theme {
  try {
    const stored = localStorage.getItem(STORAGE_KEY)
    if (stored === 'light' || stored === 'dark' || stored === 'system') {
      return stored
    }
  } catch {
    // Storage can be unavailable in a private window; the default stands.
  }
  return 'system'
}

export function storeTheme(theme: Theme) {
  try {
    localStorage.setItem(STORAGE_KEY, theme)
  } catch {
    // The choice applies to this page load even if it cannot be remembered.
  }
}

/** Whether the operating system is currently asking for a dark appearance. */
export function systemPrefersDark(): boolean {
  return window.matchMedia?.('(prefers-color-scheme: dark)').matches ?? false
}

/** Puts the theme into effect by toggling the class the CSS keys off. */
export function applyTheme(theme: Theme) {
  const dark = theme === 'dark' || (theme === 'system' && systemPrefersDark())
  document.documentElement.classList.toggle('dark', dark)
}
