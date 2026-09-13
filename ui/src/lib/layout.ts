/**
 * What the chrome around a page looks like, remembered between visits.
 *
 * Whether a panel is open is the reader's decision, not the window's. The
 * width only chooses the *default* -- a phone starts with everything shut
 * because there is no room, a desktop starts with the panels it can afford --
 * and after that a choice sticks until it is changed back.
 *
 * Per device rather than per account: which panels someone wants open is a
 * property of the screen they are looking at, so the same account on a laptop
 * and a phone should not fight over one answer.
 */

const PREFIX = 'outturn.layout.'

function read(key: string): string | null {
  try {
    return localStorage.getItem(PREFIX + key)
  } catch {
    // Storage can be unavailable in a private window; the default stands.
    return null
  }
}

function write(key: string, value: string) {
  try {
    localStorage.setItem(PREFIX + key, value)
  } catch {
    // The choice applies to this page load even if it cannot be remembered.
  }
}

export function readFlag(key: string, fallback: boolean): boolean {
  const stored = read(key)
  if (stored === 'true') return true
  if (stored === 'false') return false
  return fallback
}

export function storeFlag(key: string, value: boolean) {
  write(key, String(value))
}

/**
 * Reads a remembered choice from a known set, so a stale value left by an
 * older build -- a tab that no longer exists -- falls back rather than
 * selecting nothing.
 */
export function readOneOf<T extends string>(key: string, allowed: readonly T[], fallback: T): T {
  const stored = read(key)
  return allowed.includes(stored as T) ? (stored as T) : fallback
}

export function storeOneOf(key: string, value: string) {
  write(key, value)
}
