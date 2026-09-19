import { describe, expect, it } from 'vitest'

import { elapsedPhrase, elapsedSince, mintedAt } from './elapsed'

/** A v7 id minted at a given instant, so a test can name the time it means. */
function idAt(millis: number, tail = '7abc-8def-0123456789ab'): string {
  const hex = millis.toString(16).padStart(12, '0')
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${tail}`
}

describe('reading the instant out of an id', () => {
  it('reads a v7 timestamp back', () => {
    const when = Date.UTC(2026, 8, 19, 1, 14, 0)
    expect(mintedAt(idAt(when))).toBe(when)
  })

  it('refuses a v4 id rather than dating it', () => {
    // A v4's first 48 bits are random, so reading them as a clock gives a
    // confident answer somewhere in the wrong century.
    expect(mintedAt('9f1c2d3e-4b5a-4c6d-8e9f-0123456789ab')).toBeNull()
  })

  it('refuses anything that is not a uuid', () => {
    expect(mintedAt('')).toBeNull()
    expect(mintedAt('not-an-id')).toBeNull()
  })
})

describe('saying how long ago that was', () => {
  const now = Date.UTC(2026, 8, 19, 12, 0, 0)
  const ago = (seconds: number) => elapsedPhrase(now - seconds * 1000, now)

  it('calls anything under a minute just now', () => {
    expect(ago(0)).toBe('Just now')
    expect(ago(59)).toBe('Just now')
  })

  it('counts minutes, hours, days and beyond', () => {
    expect(ago(60)).toBe('1 minute ago')
    expect(ago(150)).toBe('2 minutes ago')
    expect(ago(3600)).toBe('1 hour ago')
    expect(ago(7200)).toBe('2 hours ago')
    expect(ago(86400)).toBe('1 day ago')
    expect(ago(86400 * 3)).toBe('3 days ago')
    expect(ago(604800)).toBe('1 week ago')
    expect(ago(2629800)).toBe('1 month ago')
    expect(ago(31557600)).toBe('1 year ago')
  })

  it('singularises one and pluralises the rest', () => {
    expect(ago(60)).toBe('1 minute ago')
    expect(ago(120)).toBe('2 minutes ago')
  })

  it('does not report a message from the future as aged', () => {
    // The browser's clock and the tier that minted the id need not agree, and
    // "in -3 minutes" is not a thing to show anybody.
    expect(elapsedPhrase(now + 5000, now)).toBe('Just now')
  })

  it('gives nothing for an id it cannot read', () => {
    expect(elapsedSince('not-an-id', now)).toBeNull()
  })

  it('reads an id end to end', () => {
    expect(elapsedSince(idAt(now - 3600 * 1000), now)).toBe('1 hour ago')
  })
})

describe('not caring what any clock reads', () => {
  // The 48-bit prefix of a v7 id is Unix milliseconds, and `Date.now()` is the
  // same scale: elapsed time since the epoch, with no zone and no offsets in
  // it. So an interval is a subtraction of two points on one absolute
  // timeline, and nothing a calendar does to wall-clock time can reach it.
  //
  // This would stop being true the moment anything here reached for
  // `getHours`, `toLocaleString` or a `Date` built from calendar parts. It
  // does not, and these hold it to that.

  it('reads an hour as an hour across a spring-forward boundary', () => {
    // Australia/Sydney springs forward at 2am on 2026-10-04, so the wall
    // clock jumps 01:59 -> 03:00. An hour either side of it is still an hour.
    const before = Date.UTC(2026, 9, 3, 15, 30) // 02:30 AEDT-to-be, in UTC
    const after = before + 3600 * 1000
    expect(elapsedPhrase(before, after)).toBe('1 hour ago')
  })

  it('reads an hour as an hour across a fall-back boundary', () => {
    // And at the other end, where a wall clock repeats an hour rather than
    // skipping one -- the naive implementations double-count here.
    const before = Date.UTC(2026, 3, 4, 15, 30)
    const after = before + 3600 * 1000
    expect(elapsedPhrase(before, after)).toBe('1 hour ago')
  })

  it('gives the same answer whatever zone the reader sits in', () => {
    // The zone never enters the arithmetic, so this is a tautology -- which is
    // the point. If it ever fails, something started consulting the locale.
    const minted = Date.UTC(2026, 8, 19, 12, 0, 0)
    const now = minted + 7200 * 1000
    const id = idAt(minted)
    for (const zone of ['UTC', 'Australia/Sydney', 'America/Los_Angeles', 'Asia/Kolkata']) {
      process.env.TZ = zone
      expect(elapsedSince(id, now)).toBe('2 hours ago')
    }
  })

  it('reads a day as a day even when that day was 23 hours long', () => {
    // A calendar day containing a spring-forward is 23 hours of real time.
    // Counting in seconds, 24 hours is a day regardless, which is the honest
    // answer for "how long ago" -- unlike a calendar difference, which would
    // call 23 elapsed hours "1 day" and 24 "1 day" too.
    const before = Date.UTC(2026, 9, 3, 12, 0)
    expect(elapsedPhrase(before, before + 23 * 3600 * 1000)).toBe('23 hours ago')
    expect(elapsedPhrase(before, before + 24 * 3600 * 1000)).toBe('1 day ago')
  })
})
