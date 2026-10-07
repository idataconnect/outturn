import { describe, expect, it } from 'vitest'

import { writtenBy } from './skills'

describe('who wrote a version, as the history says it', () => {
  it('names a person in this workspace', () => {
    expect(writtenBy({ kind: 'person', name: 'Ana' })).toBe('by Ana')
  })

  it("calls the operator's staff the operator", () => {
    expect(writtenBy({ kind: 'operator' })).toBe('by the operator')
  })

  it('says nothing where nobody was recorded, or the API is older', () => {
    expect(writtenBy({ kind: 'unrecorded' })).toBeNull()
    expect(writtenBy(undefined)).toBeNull()
  })
})
