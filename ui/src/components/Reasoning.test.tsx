import type { ReasoningMessagePartProps } from '@assistant-ui/react'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'

import Reasoning from './Reasoning'

function reasoning(text: string, running = false) {
  // Only `text` and `status` are read. The rest of a message part belongs to
  // whatever drives the thread, and building a convincing one would test the
  // library rather than this component.
  const props = {
    text,
    status: { type: running ? 'running' : 'complete' },
  } as unknown as ReasoningMessagePartProps
  return render(<Reasoning {...props} />)
}

describe('Reasoning', () => {
  it('keeps the thinking behind a disclosure, so it does not bury the answer', async () => {
    reasoning('The user wants the Orchard Room. Checking availability first.')

    expect(screen.queryByText(/Orchard Room/)).not.toBeInTheDocument()

    await userEvent.click(screen.getByRole('button'))
    expect(screen.getByText(/Orchard Room/)).toBeInTheDocument()
  })

  it('says a turn is still thinking, which may be all it is doing', () => {
    reasoning('Let me work through', true)
    expect(screen.getByText('Thinking...')).toBeInTheDocument()
  })

  it('speaks in the past tense once the turn has stopped', () => {
    reasoning('That took some working out')
    expect(screen.getByText('Thought about this')).toBeInTheDocument()
  })

  /// A model that thought about nothing should leave no trace: an empty
  /// disclosure is a control that opens onto nothing.
  it('draws nothing when there was no thinking', () => {
    const { container } = reasoning('')
    expect(container).toBeEmptyDOMElement()
  })

  it('reports whether it is open, for anybody not using a pointer', async () => {
    reasoning('some thinking')
    const toggle = screen.getByRole('button')
    expect(toggle).toHaveAttribute('aria-expanded', 'false')
    await userEvent.click(toggle)
    expect(toggle).toHaveAttribute('aria-expanded', 'true')
  })
})
