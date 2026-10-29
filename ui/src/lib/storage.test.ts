import { describe, expect, it } from 'vitest'

import { bytesLabel } from './storage'

describe('bytesLabel', () => {
  it('says a size in the unit it reads best in', () => {
    expect(bytesLabel(12, 'en-US')).toBe('12 bytes')
    expect(bytesLabel(1_500, 'en-US')).toBe('1.5 kB')
    expect(bytesLabel(340_000_000, 'en-US')).toBe('340 MB')
    expect(bytesLabel(5_000_000_000, 'en-US')).toBe('5 GB')
  })

  it('in the reader’s own way of writing numbers', () => {
    expect(bytesLabel(1_500_000, 'de-DE')).toBe('1,5 MB')
  })
})
