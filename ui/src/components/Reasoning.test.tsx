import type { ReasoningMessagePartProps } from '@assistant-ui/react'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'

import Reasoning from './Reasoning'

function reasoning(text: string, running = false, ms?: number) {
  // Only `text` and `status` are read. The rest of a message part belongs to
  // whatever drives the thread, and building a convincing one would test the
  // library rather than this component.
  const props = {
    text,
    status: { type: running ? 'running' : 'complete' },
    ...(ms === undefined ? {} : { ms }),
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

  it('says what the thought cost once the turn has stopped', () => {
    reasoning('one two three four five', false, 4200)
    expect(screen.getByText(/4\.2s/)).toBeInTheDocument()
    expect(screen.getByText(/5 words/)).toBeInTheDocument()
  })

  it('counts one word as a word', () => {
    reasoning('hm', false, 800)
    expect(screen.getByText(/1 word(?!s)/)).toBeInTheDocument()
  })

  /// Words, never tokens. Nothing here counts tokens, and a guess labelled
  /// "tokens" would be read as the number on somebody's bill.
  it('does not claim to count tokens', () => {
    reasoning('some thinking here', false, 1000)
    expect(screen.queryByText(/token/i)).not.toBeInTheDocument()
  })

  /// Rounded, because 4.23s and 4.2s are the same fact to a reader -- and past
  /// ten seconds the tenth is noise.
  it('rounds a long thought to whole seconds', () => {
    reasoning('a b c', false, 42_400)
    expect(screen.getByText(/42s/)).toBeInTheDocument()
  })

  /// A thought recorded before the duration was measured still says what it
  /// cost in the unit it can.
  it('reports the words alone when nothing timed it', () => {
    reasoning('one two three', false)
    expect(screen.getByText(/3 words/)).toBeInTheDocument()
    expect(screen.queryByText(/s \u00b7/)).not.toBeInTheDocument()
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
