import { describe, expect, it } from 'vitest'

import { remaining } from './sleep'

describe('how long an agent has left to sleep', () => {
  const now = new Date('2026-09-29T10:00:00').getTime()
  const later = (ms: number) => new Date(now + ms).toISOString()

  it('counts down in seconds, then minutes', () => {
    expect(remaining(later(45_000), now)).toBe('for 45 more seconds')
    expect(remaining(later(8 * 60_000), now)).toBe('for 8 more minutes')
    expect(remaining(later(60_000), now)).toBe('for 1 more minute')
  })

  it('gives a clock time once a count would be long', () => {
    expect(remaining(later(3 * 3_600_000), now)).toMatch(/^until \d/)
  })

  it('names the day when it is not today', () => {
    expect(remaining(later(3 * 86_400_000), now)).toMatch(/^until \D+, /)
  })

  it('says it is waking once the time has passed', () => {
    expect(remaining(later(-1_000), now)).toBe('— waking up')
  })
})
