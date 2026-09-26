import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import ActionQueue from './ActionQueue'
import type { ActionItem } from '../lib/actions'

/** A v7 id minted at a known instant, so the age phrase is deterministic. */
function idAt(millis: number): string {
  const hex = millis.toString(16).padStart(12, '0')
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-7000-8000-000000000000`
}

const NOW = Date.UTC(2026, 8, 26, 12, 0, 0)

function item(over: Partial<ActionItem> = {}): ActionItem {
  return {
    id: idAt(NOW - 5 * 60_000),
    workspace_id: 'w1',
    kind: 'hitl.approval',
    event_id: null,
    payload: { question: 'Refund order 8812?' },
    state: 'pending',
    created_at: '2026-09-26T11:55:00Z',
    expires_at: null,
    ...over,
  }
}

describe('an empty queue', () => {
  it('says nothing is waiting rather than rendering an empty list', () => {
    render(<ActionQueue items={[]} now={NOW} />)
    expect(screen.getByText('Nothing is waiting on you.')).toBeInTheDocument()
    expect(screen.queryByRole('list')).not.toBeInTheDocument()
  })

  it('takes a caller-supplied quiet line', () => {
    render(<ActionQueue items={[]} quiet="All caught up." now={NOW} />)
    expect(screen.getByText('All caught up.')).toBeInTheDocument()
  })
})

describe('a waiting item', () => {
  it('names what it is and what it is about', () => {
    render(<ActionQueue items={[item()]} now={NOW} />)
    expect(screen.getByText('Approval needed')).toBeInTheDocument()
    expect(screen.getByText(/Refund order 8812\?/)).toBeInTheDocument()
  })

  it('says how long it has been waiting rather than when it arrived', () => {
    // "waiting 5 minutes", not "5 minutes ago": the reader is being told how
    // long it has been nobody's answer.
    render(<ActionQueue items={[item()]} now={NOW} />)
    expect(screen.getByText(/waiting 5 minutes/)).toBeInTheDocument()
    expect(screen.queryByText(/ago/)).not.toBeInTheDocument()
  })

  it('renders a kind nobody wrote a label for', () => {
    // A new producer must not be a blank row.
    render(<ActionQueue items={[item({ kind: 'hitl.budget_override' })]} now={NOW} />)
    expect(screen.getByText('Budget override')).toBeInTheDocument()
  })

  it('shows no summary when the payload has no line worth showing', () => {
    render(<ActionQueue items={[item({ payload: {} })]} now={NOW} />)
    expect(screen.getByText('Approval needed')).toBeInTheDocument()
  })

  it('ignores a payload field of the wrong type', () => {
    // `payload` is free-form jsonb from a producer this component does not
    // know, and `[object Object]` on a row is worse than no summary.
    render(<ActionQueue items={[item({ payload: { question: { nested: true } } })]} now={NOW} />)
    expect(screen.queryByText(/object/i)).not.toBeInTheDocument()
  })
})

describe('which workspace a row belongs to', () => {
  it('is not shown when every row is from the same one', () => {
    // The column would be the same word repeated.
    render(
      <ActionQueue
        items={[item(), item({ id: idAt(NOW - 60_000) })]}
        workspaceNames={{ w1: 'Acme' }}
        now={NOW}
      />,
    )
    expect(screen.queryByText(/in Acme/)).not.toBeInTheDocument()
  })

  it('is shown once the queue spans more than one', () => {
    render(
      <ActionQueue
        items={[item(), item({ id: idAt(NOW - 60_000), workspace_id: 'w2' })]}
        workspaceNames={{ w1: 'Acme', w2: 'Kestrel' }}
        now={NOW}
      />,
    )
    expect(screen.getByText('in Acme')).toBeInTheDocument()
    expect(screen.getByText('in Kestrel')).toBeInTheDocument()
  })

  it('falls back to part of the id when no name was given', () => {
    render(
      <ActionQueue
        items={[item({ workspace_id: 'abcdef1234' }), item({ workspace_id: 'w2' })]}
        now={NOW}
      />,
    )
    expect(screen.getByText('in abcdef12')).toBeInTheDocument()
  })
})

describe('expiry', () => {
  it('is not shown while it is far off', () => {
    // An expiry a week out is noise on every row.
    const far = new Date(NOW + 7 * 86_400_000).toISOString()
    render(<ActionQueue items={[item({ expires_at: far })]} now={NOW} />)
    expect(screen.queryByText(/expires/)).not.toBeInTheDocument()
  })

  it('is shown once it is close enough to change what to do next', () => {
    const soon = new Date(NOW + 10 * 60_000).toISOString()
    render(<ActionQueue items={[item({ expires_at: soon })]} now={NOW} />)
    expect(screen.getByText(/expires in 10 mins/)).toBeInTheDocument()
  })

  it('says so when it has already passed', () => {
    const gone = new Date(NOW - 60_000).toISOString()
    render(<ActionQueue items={[item({ expires_at: gone })]} now={NOW} />)
    expect(screen.getByText('expired')).toBeInTheDocument()
  })
})

describe('opening an item', () => {
  it('offers no affordance when there is nowhere to go', () => {
    // A row that looks clickable and is not is worse than one that never
    // offered.
    render(<ActionQueue items={[item()]} now={NOW} />)
    expect(screen.queryByRole('button')).not.toBeInTheDocument()
  })

  it('opens on a click when a handler was given', async () => {
    const onOpen = vi.fn()
    render(<ActionQueue items={[item()]} onOpen={onOpen} now={NOW} />)
    await userEvent.click(screen.getByRole('button'))
    expect(onOpen).toHaveBeenCalledTimes(1)
    expect(onOpen.mock.calls[0][0].kind).toBe('hitl.approval')
  })

  it('opens from the keyboard, since a div with a button role gets nothing free', async () => {
    const onOpen = vi.fn()
    render(<ActionQueue items={[item()]} onOpen={onOpen} now={NOW} />)
    const row = screen.getByRole('button')
    row.focus()
    await userEvent.keyboard('{Enter}')
    expect(onOpen).toHaveBeenCalledTimes(1)
    await userEvent.keyboard(' ')
    expect(onOpen).toHaveBeenCalledTimes(2)
  })
})
