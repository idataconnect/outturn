import { describe, expect, it } from 'vitest'

import { annotate, withQuote } from './useChatRuntime'
import type { Message } from './chat'

/** A stored message, with only what `annotate` reads. */
function message(over: Partial<Message> & Pick<Message, 'id' | 'role'>): Message {
  return {
    content: '',
    metadata: {},
    delta_next: 0,
    model: null,
    replies_to: null,
    absorbed_by: null,
    job_state: null,
    ...over,
  }
}

const statusOf = (msgs: Message[], id: string) =>
  annotate(msgs, new Set(), new Map()).find((m) => m.id === id)?.status ?? null

describe('a prompt whose reply never arrives', () => {
  it('is still waiting while the turn is running', () => {
    const msgs = [
      message({ id: 'u1', role: 'user', content: 'go', job_state: 'running' }),
      message({ id: 'a1', role: 'assistant', replies_to: 'u1' }),
    ]
    expect(statusOf(msgs, 'u1')).toEqual({ kind: 'waiting' })
  })

  it('says the turn ended silently once the job is done', () => {
    // What gemma4 did: spent the turn on tool calls that went nowhere and
    // wrote nothing. The job succeeded, so no event is coming -- and calling
    // this "waiting" left a spinner running for a reply that never arrives.
    const msgs = [
      message({ id: 'u1', role: 'user', content: 'go', job_state: 'succeeded' }),
      message({ id: 'a1', role: 'assistant', replies_to: 'u1' }),
    ]
    expect(statusOf(msgs, 'u1')).toEqual({ kind: 'silent' })
  })

  it('is not called silent while it is being retried', () => {
    // A retry reuses the same empty reply, so emptiness here means "about to
    // start over", not "said nothing".
    const msgs = [
      message({ id: 'u1', role: 'user', content: 'go', job_state: 'succeeded' }),
      message({ id: 'a1', role: 'assistant', replies_to: 'u1' }),
    ]
    const annotated = annotate(msgs, new Set(['a1']), new Map())
    expect(annotated.find((m) => m.id === 'u1')?.status).toEqual({ kind: 'retrying' })
  })

  it('leaves a reply that has content alone', () => {
    const msgs = [
      message({ id: 'u1', role: 'user', content: 'go', job_state: 'succeeded' }),
      message({ id: 'a1', role: 'assistant', replies_to: 'u1', content: 'here you are' }),
    ]
    expect(statusOf(msgs, 'u1')).toBeNull()
  })

  it('leaves a reply that is only tool calls alone', () => {
    // Mid-turn: nothing written yet, but the agent is visibly working.
    const msgs = [
      message({ id: 'u1', role: 'user', content: 'go', job_state: 'running' }),
      message({
        id: 'a1',
        role: 'assistant',
        replies_to: 'u1',
        metadata: {
          tool_calls: [{ id: 'c1', name: 'fetch_url', action: 'Looking something up' }],
        },
      }),
    ]
    expect(statusOf(msgs, 'u1')).toBeNull()
  })

  it('still reports a failure as a failure', () => {
    const msgs = [
      message({ id: 'u1', role: 'user', content: 'go', job_state: 'failed' }),
      message({ id: 'a1', role: 'assistant', replies_to: 'u1' }),
    ]
    expect(statusOf(msgs, 'u1')).toEqual({ kind: 'failed', message: 'the turn failed' })
  })
})

describe('quoting a passage out of a reply', () => {
  it('sends the quote with the message, as markdown', () => {
    const sent = withQuote('is this still true?', {
      quote: { text: 'The ledger gets a row per model call.' },
    })

    // `>` rather than a structure: the model already knows what it means, and
    // the transcript reads back the way it was written.
    expect(sent).toBe('> The ledger gets a row per model call.\n\nis this still true?')
  })

  it('marks every line of a passage, not just the first', () => {
    const sent = withQuote('why?', { quote: { text: 'One line.\nAnd another.' } })

    // One `>` on a multi-line selection quotes exactly one line and leaves
    // the rest reading as though the person had said it themselves.
    expect(sent).toBe('> One line.\n> And another.\n\nwhy?')
  })

  it('leaves a message alone when nothing was quoted', () => {
    expect(withQuote('hello', undefined)).toBe('hello')
    expect(withQuote('hello', {})).toBe('hello')
    // Whitespace is not a quote: dismissing one must not leave a stray `>`
    // at the top of the next message.
    expect(withQuote('hello', { quote: { text: '   ' } })).toBe('hello')
  })
})
