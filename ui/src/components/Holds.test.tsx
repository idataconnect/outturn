import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import Holds from './Holds'
import type { Inhibitor } from '../lib/inhibitors'

function hold(over: Partial<Inhibitor> = {}): Inhibitor {
  return {
    id: crypto.randomUUID(),
    scope: { level: 'workspace', workspace_id: 'w' },
    strength: 'stopped',
    reason: 'monthly spend cap reached',
    held_by: 'billing-bot',
    created_at: '2026-09-17T06:00:00Z',
    ...over,
  }
}

describe('showing what is held', () => {
  it('says nothing when nothing is held and no quiet line was given', () => {
    const { container } = render(
      <Holds held={[]} canRelease={() => true} onRelease={() => {}} />,
    )
    expect(container).toBeEmptyDOMElement()
  })

  it('shows every hold, not only the strongest', () => {
    // A workspace both stopped and waiting on somebody must not report only
    // the stop: that hides a request still expected to be answered.
    render(
      <Holds
        held={[hold(), hold({ strength: 'suspended', reason: 'waiting on approval' })]}
        canRelease={() => true}
        onRelease={() => {}}
      />,
    )

    expect(screen.getByText(/monthly spend cap reached/)).toBeInTheDocument()
    expect(screen.getByText(/waiting on approval/)).toBeInTheDocument()
  })

  it('says what each hold covers, so a workspace hold is not read as an agent one', () => {
    render(
      <Holds
        held={[hold(), hold({ scope: { level: 'agent', workspace_id: 'w', agent_id: 'a' } })]}
        canRelease={() => true}
        onRelease={() => {}}
      />,
    )

    expect(screen.getByText(/This whole workspace: stopped/)).toBeInTheDocument()
    expect(screen.getByText(/This agent: stopped/)).toBeInTheDocument()
  })

  it('names who is holding it and since when', () => {
    render(<Holds held={[hold()]} canRelease={() => true} onRelease={() => {}} />)
    expect(screen.getByText(/billing-bot/)).toBeInTheDocument()
  })

  it('offers no release to somebody who may not lift it', () => {
    render(<Holds held={[hold()]} canRelease={() => false} onRelease={() => {}} />)
    expect(screen.queryByRole('button')).not.toBeInTheDocument()
  })

  it('releases the hold that was clicked', async () => {
    const onRelease = vi.fn()
    const first = hold({ reason: 'first' })
    const second = hold({ reason: 'second' })
    render(<Holds held={[first, second]} canRelease={() => true} onRelease={onRelease} />)

    await userEvent.click(screen.getByRole('button', { name: 'Release: second' }))

    expect(onRelease).toHaveBeenCalledTimes(1)
    expect(onRelease).toHaveBeenCalledWith(second)
  })
})
