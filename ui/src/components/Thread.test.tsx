import { render, screen } from '@testing-library/react'
import {
  AssistantRuntimeProvider,
  useExternalStoreRuntime,
  type ThreadMessageLike,
} from '@assistant-ui/react'
import { describe, expect, it } from 'vitest'

import Thread from './Thread'

function Harness(props: { disabled?: boolean; focusRequest?: number }) {
  const runtime = useExternalStoreRuntime<ThreadMessageLike>({
    messages: [],
    convertMessage: (message) => message,
    onNew: async () => {},
  })
  return (
    <AssistantRuntimeProvider runtime={runtime}>
      <button type="button">elsewhere</button>
      <Thread {...props} />
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
