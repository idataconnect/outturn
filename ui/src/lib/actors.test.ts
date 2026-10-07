import { describe, expect, it } from 'vitest'

import { actorName } from './actors'

describe('who did something, in words', () => {
  it('names a person', () => {
    expect(actorName({ kind: 'person', name: 'Ana' })).toBe('Ana')
  })

  it("calls the operator's staff the operator", () => {
    expect(actorName({ kind: 'operator' })).toBe('the operator')
  })

  it('describes a rule or a machine as the API worded it', () => {
    expect(actorName({ kind: 'system', name: 'a rule that needs approval' })).toBe(
      'a rule that needs approval',
    )
  })

  it('says nothing where nobody was recorded, or the API is older', () => {
    expect(actorName({ kind: 'unrecorded' })).toBeNull()
    expect(actorName(undefined)).toBeNull()
  })
})
