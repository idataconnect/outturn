import { describe, expect, it } from 'vitest'

import { annotate, splitAtSteers, withQuote } from './useChatRuntime'
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

describe('a failed turn put back on the queue', () => {
  const failed = [
    message({ id: 'u1', role: 'user', job_state: 'failed' }),
    message({ id: 'a1', role: 'assistant', replies_to: 'u1' }),
  ]

  it('shows the retry button while it is failed', () => {
    const status = annotate(failed, new Set(), new Map([['u1', 'the model was unreachable']]))
      .find((m) => m.id === 'u1')?.status
    expect(status).toEqual({ kind: 'failed', message: 'the model was unreachable' })
  })

  it('needs both the failure and the stored state cleared, not either', () => {
    // `annotate` reads the map *or* job_state, so clearing one leaves the
    // button sitting beside a turn that is already running again -- which is
    // exactly what the first version of this did.
    const mapOnly = annotate(failed, new Set(), new Map()).find((m) => m.id === 'u1')?.status
    expect(mapOnly).toMatchObject({ kind: 'failed' })

    const stateOnly = annotate(
      [message({ id: 'u1', role: 'user', job_state: 'pending' }), failed[1]],
      new Set(),
      new Map([['u1', 'the model was unreachable']]),
    ).find((m) => m.id === 'u1')?.status
    expect(stateOnly).toMatchObject({ kind: 'failed' })
  })

  it('goes back to waiting once both are cleared', () => {
    const status = annotate(
      [message({ id: 'u1', role: 'user', job_state: 'pending' }), failed[1]],
      new Set(),
      new Map(),
    ).find((m) => m.id === 'u1')?.status

    // The held mark, which is what somebody who pressed the button expects to
    // see: the turn is going again, and nothing about it has failed yet.
    expect(status).toMatchObject({ kind: 'waiting' })
  })
})

describe('a reply that has not said anything yet', () => {
  const pair = (job_state: Message['job_state']) => [
    message({ id: 'u1', role: 'user', job_state }),
    message({ id: 'a1', role: 'assistant', replies_to: 'u1' }),
  ]

  it('is waiting, not silent, before its job row exists', () => {
    // `job_state` is null between storing the message and enqueueing its
    // turn. Reading that as "the job is over" called the agent silent a
    // moment before it began streaming -- next to a stop button saying a
    // turn was running.
    expect(annotate(pair(null), new Set(), new Map()).find((m) => m.id === 'u1')?.status)
      .toMatchObject({ kind: 'waiting' })
  })

  it('is waiting while the turn runs', () => {
    expect(annotate(pair('running'), new Set(), new Map()).find((m) => m.id === 'u1')?.status)
      .toMatchObject({ kind: 'waiting' })
  })

  it('is silent once the turn ended having said nothing', () => {
    // The case this state exists for: a turn that spent itself on tool calls
    // going nowhere, or a model that answered with nothing at all.
    expect(annotate(pair('succeeded'), new Set(), new Map()).find((m) => m.id === 'u1')?.status)
      .toMatchObject({ kind: 'silent' })
  })
})

describe('a message taken into a reply mid-turn', () => {
  // The shape of a real session: a question, a reply whose first round was a
  // tool call, and a second question the agent took at that round boundary
  // and answered in the rest of the same reply.
  const session = () => [
    message({ id: 'u1', role: 'user', content: 'What day is it?', job_state: 'succeeded' }),
    message({
      id: 'a1',
      role: 'assistant',
      replies_to: 'u1',
      content: 'Checking.\n\nI can do lots.',
      metadata: {
        tool_calls: [
          { id: 'c1', name: 'load_tools', action: 'Loading', details: '{}' },
          { id: 'c2', name: 'get_current_time', action: 'Clock', details: '{}' },
        ],
        parts: [
          { type: 'text', text: 'Checking.' },
          { type: 'call', id: 'c1' },
          { type: 'steer', id: 'u2' },
          { type: 'call', id: 'c2' },
          { type: 'text', text: '\n\nI can do lots.' },
        ],
      },
    }),
    message({
      id: 'u2',
      role: 'user',
      content: 'What else can you do?',
      absorbed_by: 'a1',
      job_state: 'succeeded',
    }),
  ]
  const drawn = (msgs: Message[]) => splitAtSteers(annotate(msgs, new Set(), new Map()))

  it('is drawn between the reply so far and the rest of it', () => {
    const out = drawn(session())

    expect(out.map((m) => m.id)).toEqual(['u1', 'a1', 'u2', 'a1:1'])
    expect(out[1].metadata.parts).toEqual([
      { type: 'text', text: 'Checking.' },
      { type: 'call', id: 'c1' },
    ])
    expect(out[1].metadata.tool_calls?.map((c) => c.id)).toEqual(['c1'])
    // The round separator starts the new box rather than sitting above it.
    expect(out[3].metadata.parts).toEqual([
      { type: 'call', id: 'c2' },
      { type: 'text', text: 'I can do lots.' },
    ])
    expect(out[3].metadata.tool_calls?.map((c) => c.id)).toEqual(['c2'])
    // Where it sits says it was taken; a badge saying so as well is noise.
    expect(out[2].status).toBeNull()
  })

  it('gives the mark and the live state only to the box being written', () => {
    const msgs = session()
    msgs[0].job_state = 'running'
    const out = drawn(msgs)

    expect(out.find((m) => m.id === 'a1')).toMatchObject({ newest: false, live: false })
    expect(out.find((m) => m.id === 'a1:1')).toMatchObject({ newest: true, live: true })
  })

  it('says the agent is on it until the answer has begun', () => {
    // Taken, but the model has not said anything back yet: the box after it
    // is empty and not drawn, so the message carries the waiting mark.
    const msgs = session()
    msgs[0].job_state = 'running'
    msgs[1].metadata.parts = msgs[1].metadata.parts!.slice(0, 3)
    const out = drawn(msgs)

    expect(out.find((m) => m.id === 'u2')?.status).toEqual({ kind: 'waiting' })
  })

  it('is left where it was when the message is not on the page', () => {
    // Paging back can load a reply without the message it took; splitting
    // around nothing would be a break with no reason shown.
    const msgs = session().filter((m) => m.id !== 'u2')

    expect(drawn(msgs).map((m) => m.id)).toEqual(['u1', 'a1'])
  })
})
