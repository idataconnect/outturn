/**
 * How long ago a message was written, worked out when somebody asks.
 *
 * Never stored and never cached. A relative time is wrong the moment after it
 * is computed, so the only safe place to compute one is at the point of
 * display: what is kept is the instant the message was written, which does not
 * change, and the phrase is derived from it on demand.
 */

/**
 * When a UUIDv7 was minted, in Unix milliseconds.
 *
 * Read out of the id rather than from a column beside it. Ordering in this
 * system already rides on these keys -- no sequences, no separate timestamp --
 * so the id is the authoritative instant, and a `created_at` alongside it would
 * be a second copy that could disagree.
 *
 * The first 48 bits are the millisecond clock. Anything that is not a v7 uuid
 * returns null rather than a plausible date: a wrong time shown confidently is
 * worse than no time at all.
 */
export function mintedAt(id: string): number | null {
  // 8-4-4-4-12 hex, with the version nibble pinned to 7. A v4 id would parse
  // happily as a timestamp and land in 1970 or the far future.
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id)) {
    return null
  }
  const millis = Number.parseInt(id.slice(0, 8) + id.slice(9, 13), 16)
  return Number.isFinite(millis) ? millis : null
}

/**
 * How long ago that was, as a person would say it.
 *
 * Coarse on purpose. Somebody hovering a message wants to know whether it was
 * a moment ago or last Tuesday, not that it was 4 minutes and 12 seconds ago --
 * and a phrase precise enough to be interesting is precise enough to be stale.
 *
 * Anything under a minute is "Just now", which is both what a reader means by
 * it and the honest limit of a figure computed once and then left on screen.
 */
export function elapsedPhrase(since: number, now: number = Date.now()): string {
  const seconds = Math.floor((now - since) / 1000)

  // A message from the future is a clock disagreement between this browser and
  // the tier that minted the id, not a fact worth rendering. Say the least
  // wrong thing.
  if (seconds < 60) return 'Just now'

  const units: [limit: number, seconds: number, name: string][] = [
    [60, 60, 'minute'],
    [24, 3600, 'hour'],
    [7, 86400, 'day'],
    // Four, not five: a month is 4.35 weeks, so a limit of five would report
    // "4 weeks ago" for something the next row calls a month.
    [4, 604800, 'week'],
    [12, 2629800, 'month'],
    [Number.POSITIVE_INFINITY, 31557600, 'year'],
  ]

  for (const [limit, size, name] of units) {
    const count = Math.floor(seconds / size)
    if (count < limit) {
      return `${count} ${name}${count === 1 ? '' : 's'} ago`
    }
  }

  return 'Just now'
}

/**
 * The phrase for a message id, or null when there is nothing to say.
 *
 * Null rather than a fallback string, so a caller renders nothing at all for an
 * id it cannot read instead of a tooltip that says "unknown".
 */
export function elapsedSince(id: string, now: number = Date.now()): string | null {
  const minted = mintedAt(id)
  return minted === null ? null : elapsedPhrase(minted, now)
}
