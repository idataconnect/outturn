import type { ReasoningMessagePartProps } from '@assistant-ui/react'
import { act, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'

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

  /// Words, never tokens. Nothing here counts tokens, and a guess labeled
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

describe('a thought still being written', () => {
  afterEach(() => vi.useRealTimers())

  function live(text: string, ms: number, seenAt: number) {
    const props = {
      text,
      status: { type: 'running' },
      ms,
      seenAt,
    } as unknown as ReasoningMessagePartProps
    return render(<Reasoning {...props} />)
  }

  it('counts its time up between fragments, from what was last measured', () => {
    vi.useFakeTimers()
    const t0 = new Date('2026-09-30T12:00:00Z').getTime()
    vi.setSystemTime(t0)
    // Measured at 1.2s when the last fragment arrived, this instant.
    live('one two three', 1200, t0)
    expect(screen.getByText(/1\.2s · 3 words/)).toBeInTheDocument()

    act(() => {
      vi.advanceTimersByTime(800)
    })
    expect(screen.getByText(/2\.0s · 3 words/)).toBeInTheDocument()
  })

  /// What was seen: a thought followed by a tool call being written kept
  /// counting through seconds of silence, then dropped to its real figure
  /// when the call appeared.
  it('stops counting once the stream goes quiet, at what was measured', () => {
    vi.useFakeTimers()
    const t0 = new Date('2026-09-30T12:00:00Z').getTime()
    vi.setSystemTime(t0)
    live('one two three', 4200, t0)
    act(() => {
      vi.advanceTimersByTime(3000)
    })
    expect(screen.getByText(/4\.2s · 3 words/)).toBeInTheDocument()
  })

  it('counts words as they arrive', () => {
    vi.useFakeTimers()
    const t0 = Date.now()
    const { rerender } = live('one two', 0, t0)
    expect(screen.getByText(/2 words/)).toBeInTheDocument()
    rerender(
      <Reasoning
        {...({ text: 'one two three four', status: { type: 'running' }, ms: 300, seenAt: t0 } as unknown as ReasoningMessagePartProps)}
      />,
    )
    expect(screen.getByText(/4 words/)).toBeInTheDocument()
  })

  it('stops at the measured figure once it is done', () => {
    vi.useFakeTimers()
    reasoning('one two three', false, 1200)
    act(() => {
      vi.advanceTimersByTime(5000)
    })
    expect(screen.getByText(/1\.2s/)).toBeInTheDocument()
  })
})
