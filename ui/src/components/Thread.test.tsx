import { render, screen } from '@testing-library/react'
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
  focusRequest?: number
  running?: boolean
  messages?: ThreadMessageLike[]
}) {
  const runtime = useExternalStoreRuntime<ThreadMessageLike>({
    messages: props.messages ?? [],
    convertMessage: (message) => message,
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
      <Thread disabled={props.disabled} focusRequest={props.focusRequest} />
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
})
