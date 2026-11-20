import { describe, expect, it } from 'vitest'

import { dictationFailure } from './dictation'

describe('dictationFailure', () => {
  it('says nothing for an ending that was not an error', () => {
    expect(dictationFailure(undefined)).toBeUndefined()
    expect(dictationFailure({ reason: 'stopped' })).toBeUndefined()
    expect(dictationFailure({ reason: 'cancelled' })).toBeUndefined()
  })

  it('names a blocked microphone', () => {
    const end = { reason: 'error', error: { code: 'not-allowed', message: '' } } as const
    expect(dictationFailure(end)).toBe('Microphone access is blocked for this site.')
  })

  it('still explains an error it has no wording for', () => {
    expect(dictationFailure({ reason: 'error' })).toBe('Dictation stopped because of an error.')
    const end = { reason: 'error', error: { code: 'something-new', message: '' } } as const
    expect(dictationFailure(end)).toBe('Dictation stopped because of an error.')
  })
})
