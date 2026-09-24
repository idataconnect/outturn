import { fireEvent, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import {
  AssistantRuntimeProvider,
  useExternalStoreRuntime,
  type ThreadMessageLike,
} from '@assistant-ui/react'
import { describe, expect, it } from 'vitest'

import Thread from './Thread'

function Harness(props: {
  disabled?: boolean
  readOnly?: boolean
  focusRequest?: number
  running?: boolean
  messages?: ThreadMessageLike[]
  sessionId?: string
}) {
  const runtime = useExternalStoreRuntime<ThreadMessageLike>({
    messages: props.messages ?? [],
    // The mark is worn by the newest reply, which the page's own runtime
    // marks in `annotate`. Without it every fixture reply looks like an older
    // one and draws nothing.
    convertMessage: (message, index) => ({
      ...message,
      metadata: {
        ...message.metadata,
        custom: {
          ...message.metadata?.custom,
          // The page's runtime sets this from the reply's job; a fixture may
          // say it that way or on the message's own status.
          live:
            message.status?.type === 'running' || message.metadata?.custom?.live === true,
          newest:
            message.role === 'assistant' &&
            index === (props.messages ?? []).findLastIndex((m) => m.role === 'assistant'),
        },
      },
    }),
    onNew: async () => {},
    isRunning: props.running,
    // `canCancel` is false without one, which is the whole of what decides
    // whether the stop is ever shown.
    onCancel: async () => {},
    // What the page's runtime has, and what keeps send available mid-reply:
    // assistant-ui disables it during a run unless the runtime can queue, and
    // a message sent then steers the turn rather than waiting for it.
    queue: {
      items: [],
      steerItems: [],
      enqueue: () => {},
      steer: () => {},
      move: () => {},
      edit: () => {},
      remove: () => {},
    },
  })
  return (
    <AssistantRuntimeProvider runtime={runtime}>
      <button type="button">elsewhere</button>
      {/* `messages` is the harness's own, for the store above: passing it on
          would put an unknown prop on the component under test. */}
      <Thread
        disabled={props.disabled}
        readOnly={props.readOnly}
        focusRequest={props.focusRequest}
        sessionId={props.sessionId}
      />
    </AssistantRuntimeProvider>
  )
}

const composer = () => screen.getByPlaceholderText('Message the agent…')

describe('focusing the composer', () => {
  it('takes the cursor when a focus is requested', () => {
    const { rerender } = render(<Harness focusRequest={0} />)
    // Where the click that started a session leaves it.
    screen.getByRole('button', { name: 'elsewhere' }).focus()

    rerender(<Harness focusRequest={1} />)

    expect(composer()).toHaveFocus()
  })

  it('does not take it back on a render that asked for nothing', () => {
    const { rerender } = render(<Harness focusRequest={1} />)
    const elsewhere = screen.getByRole('button', { name: 'elsewhere' })
    elsewhere.focus()

    rerender(<Harness focusRequest={1} />)

    expect(elsewhere).toHaveFocus()
  })

  it('waits for the composer to be enabled, for the first session of all', () => {
    const { rerender } = render(<Harness disabled focusRequest={0} />)
    screen.getByRole('button', { name: 'elsewhere' }).focus()

    // The request lands a render before the session it was for.
    rerender(<Harness disabled focusRequest={1} />)
    rerender(<Harness focusRequest={1} />)

    expect(composer()).toHaveFocus()
  })
})

// Nothing to send is the only state in which the stop is the more useful of
// the two: a message typed mid-reply steers the turn rather than waiting for
// it, so the send button is what somebody typing then is reaching for.
describe('choosing between send and stop', () => {
  const send = () => screen.queryByRole('button', { name: 'Send' })
  const stop = () => screen.queryByRole('button', { name: 'Stop' })

  it('offers a disabled send when there is nothing to send and nothing running', () => {
    render(<Harness />)

    expect(stop()).not.toBeInTheDocument()
    expect(send()).toBeDisabled()
  })

  it('enables send once something is typed', async () => {
    render(<Harness />)

    await userEvent.type(composer(), 'hello')

    expect(send()).toBeEnabled()
  })

  it('offers the stop while a turn runs and the box is empty', () => {
    render(<Harness running />)

    expect(send()).not.toBeInTheDocument()
    expect(stop()).toBeInTheDocument()
  })

  it('swaps the stop for a send once something is typed mid-reply', async () => {
    render(<Harness running />)

    await userEvent.type(composer(), 'and also')

    expect(stop()).not.toBeInTheDocument()
    expect(send()).toBeEnabled()
  })

  it('brings the stop back when the box is cleared again', async () => {
    render(<Harness running />)

    await userEvent.type(composer(), 'never mind')
    await userEvent.clear(composer())

    expect(send()).not.toBeInTheDocument()
    expect(stop()).toBeInTheDocument()
  })
})

// The gap this covers is the one after some text has arrived and before the
// turn ends -- a tool call being set up streams nothing, and the reply sits
// there looking finished. The status under the prompt has stopped speaking for
// it by then, because the reply exists.
describe('saying a reply is still going', () => {
  const working = () => screen.queryByRole('status', { name: 'Working' })
  const reply = (status: ThreadMessageLike['status']): ThreadMessageLike[] => [
    { role: 'user', content: [{ type: 'text', text: 'hello' }] },
    { role: 'assistant', content: [{ type: 'text', text: 'looking now' }], status },
  ]

  it('marks a reply whose turn is still running', () => {
    render(<Harness running messages={reply({ type: 'running' })} />)

    expect(working()).toBeInTheDocument()
  })

  it('drops the mark once the turn is complete', () => {
    render(<Harness messages={reply({ type: 'complete', reason: 'stop' })} />)

    expect(working()).not.toBeInTheDocument()
  })

  it('drops it on a turn that was stopped, which is not still going', () => {
    render(<Harness messages={reply({ type: 'incomplete', reason: 'cancelled' })} />)

    expect(working()).not.toBeInTheDocument()
  })

  it('keeps the mark moving on a reply while a message queues behind it', () => {
    // A message queued mid-reply wears the hourglass, not a second mark. And
    // the reply keeps moving: assistant-ui marks only the thread's last
    // message running, and the queued one is last, so reading its status
    // settled the reply into its finished line mid-stream.
    render(
      <Harness
        running
        messages={[
          { role: 'user', content: [{ type: 'text', text: 'first' }] },
          {
            role: 'assistant',
            content: [{ type: 'text', text: 'half an answ' }],
            // No status of its own, as the page's runtime sends it.
            metadata: { custom: { live: true } },
          },
          {
            role: 'user',
            content: [{ type: 'text', text: 'and also' }],
            metadata: { custom: { status: { kind: 'steering' } } },
          },
        ]}
      />,
    )

    expect(screen.queryAllByRole('status', { name: /Working|Finished/ })).toHaveLength(1)
    expect(screen.getByRole('status', { name: /Working/ })).toBeInTheDocument()
    expect(screen.getByRole('status', { name: /^Queued/ })).toBeInTheDocument()
  })

  it('draws one mark when an older reply is the one still running', () => {
    // A turn that crashed mid-generation leaves a reply the agent resumes
    // when the next message arrives -- so the older reply runs while a newer
    // one already exists. Drawn on `running || newest`, that was two marks at
    // once: a settled line below and a travelling swell above it.
    render(
      <Harness
        running
        messages={[
          { role: 'user', content: [{ type: 'text', text: 'first' }] },
          {
            role: 'assistant',
            content: [{ type: 'text', text: 'half an answ' }],
            status: { type: 'running' },
          },
          { role: 'user', content: [{ type: 'text', text: 'second' }] },
          {
            role: 'assistant',
            content: [{ type: 'text', text: 'and the next' }],
            status: { type: 'complete', reason: 'stop' },
          },
        ]}
      />,
    )

    // The mark says where the conversation is, and that is its last reply
    // whichever one the agent happens to be finishing.
    expect(screen.queryAllByRole('status', { name: /Working|Finished/ })).toHaveLength(1)
  })
})

describe('dropping a file on the composer', () => {
  /** A drag carrying files, which is the only kind the composer claims. */
  function fileDrag(files: File[]) {
    return {
      dataTransfer: {
        types: ['Files'],
        files,
        items: files.map((file) => ({ kind: 'file', type: file.type })),
      },
    }
  }

  it('offers to take a dragged file', () => {
    render(<Harness sessionId="s1" />)

    fireEvent.dragOver(composer(), fileDrag([new File(['x'], 'notes.pdf')]))

    // The outline is on the box rather than over the conversation: the drop
    // lands here, and saying so where it lands is less startling than
    // covering what somebody is reading to say it.
    expect(composer().closest('.ring-2')).not.toBeNull()
  })

  it('takes the outline back when the drag leaves again', () => {
    render(<Harness sessionId="s1" />)
    const box = composer()

    fireEvent.dragOver(box, fileDrag([new File(['x'], 'notes.pdf')]))
    fireEvent.dragLeave(box, { relatedTarget: document.body })

    expect(box.closest('.ring-2')).toBeNull()
  })

  it('ignores a drag that is not carrying files', () => {
    render(<Harness sessionId="s1" />)

    // Dragging selected text within the box is an ordinary edit. Claiming it
    // would break moving a word from one end of a sentence to the other.
    fireEvent.dragOver(composer(), {
      dataTransfer: { types: ['text/plain'], files: [], items: [] },
    })

    expect(composer().closest('.ring-2')).toBeNull()
  })

  it('refuses a drop before there is a session to store it in', () => {
    render(<Harness />)

    fireEvent.dragOver(composer(), fileDrag([new File(['x'], 'notes.pdf')]))

    // Nothing to attach it to yet, so the box must not say it will take it.
    expect(composer().closest('.ring-2')).toBeNull()
  })
})

describe('a reader who may not send', () => {
  it('does not tell them to start a session they already have', () => {
    render(<Harness sessionId="s1" readOnly />)

    // The one thing the box must not say. It is advice, and acting on it is
    // impossible for somebody who already has a session -- it would send them
    // looking for the thing they are already looking at.
    expect(screen.queryByPlaceholderText('Start a session first')).not.toBeInTheDocument()
  })

  it('will not take a message it cannot send', () => {
    // Both, as the page passes them.
    render(<Harness sessionId="s1" readOnly disabled />)

    // The box used to accept a paragraph, clear itself on submit, and fail --
    // so the writing was gone and the only trace was a banner in the top bar.
    // Refusing the keystroke loses nothing.
    expect(screen.getByRole('textbox')).toBeDisabled()
  })
})

describe('copying a reply', () => {
  const reply = (status: ThreadMessageLike['status']): ThreadMessageLike[] => [
    { role: 'user', content: [{ type: 'text', text: 'hello' }] },
    { role: 'assistant', content: [{ type: 'text', text: 'the answer' }], status },
  ]

  it('offers a copy button on a finished reply', () => {
    render(<Harness messages={reply({ type: 'complete', reason: 'stop' })} />)

    // Selecting by hand takes the age and the working mark with it, so the
    // text somebody gets is never quite the reply.
    expect(screen.getByRole('button', { name: 'Copy this reply' })).toBeInTheDocument()
  })

  it('does not offer it while the reply is still being written', () => {
    render(<Harness running messages={reply({ type: 'running' })} />)

    // Half a sentence is not what anybody means to copy.
    expect(screen.queryByRole('button', { name: 'Copy this reply' })).not.toBeInTheDocument()
  })

  it('shows and hides with the age beside it', async () => {
    const user = userEvent.setup()
    render(<Harness messages={reply({ type: 'complete', reason: 'stop' })} />)

    const bar = screen.getByRole('button', { name: 'Copy this reply' }).parentElement!
    expect(bar.className).toContain('opacity-0')

    // One hover region for the whole reply, not two conditions that can
    // disagree: the age is on a timer and the copy button was on a CSS
    // hover, so a long hover left the icon beside nothing.
    await user.hover(screen.getByText('the answer'))

    expect(bar.className).toContain('opacity-100')
  })
})
