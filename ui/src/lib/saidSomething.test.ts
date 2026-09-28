import { describe, expect, it } from 'vitest'

import { saidSomething } from './chat'
import type { Message } from './chat'

const reply = (over: Partial<Message>): Pick<Message, 'content' | 'metadata'> => ({
  content: '',
  metadata: {},
  ...over,
})

describe('whether a reply has anything in it', () => {
  it('counts the words', () => {
    expect(saidSomething(reply({ content: 'here you go' }))).toBe(true)
  })

  /// The term the second copy of this rule had lost. A failed turn's reply that
  /// made calls and said nothing vanished from the page while surviving in the
  /// transcript, and reappeared on the next reload.
  it('counts the calls it made, with no words at all', () => {
    expect(
      saidSomething(
        reply({ metadata: { tool_calls: [{ id: 't1', name: 'fetch_url', action: 'Checking' }] } }),
      ),
    ).toBe(true)
  })

  /// A turn the model spent deliberating. The thought is the only account of
  /// where its tokens went, so discarding it loses the explanation.
  it('counts thinking, which is not an answer but is not nothing', () => {
    expect(
      saidSomething(reply({ metadata: { parts: [{ type: 'reasoning', text: 'hmm' }] } })),
    ).toBe(true)
  })

  it('is false for a reply that is genuinely empty', () => {
    expect(saidSomething(reply({}))).toBe(false)
    expect(saidSomething(reply({ metadata: { parts: [] } }))).toBe(false)
  })

  /// A steer is a message arriving, not the agent speaking.
  it('does not count a steer as the agent having replied', () => {
    expect(
      saidSomething(reply({ metadata: { parts: [{ type: 'steer', id: 'u2' }] } })),
    ).toBe(false)
  })
})
