import type { ToolCallMessagePartProps } from '@assistant-ui/react'
import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import ToolTimer from './ToolTimer'

function timer(toolName: string, details: Record<string, unknown>) {
  // As in ToolClock's tests: only `toolName` and `args` are read.
  const props = {
    toolName,
    args: { action: 'Setting a reminder', details: JSON.stringify(details) },
  } as unknown as ToolCallMessagePartProps
  return render(<ToolTimer {...props} />)
}

const at = { when: 'Tuesday 29 September at 3:12 pm PDT', in: '59 minutes' }

describe('confirming a timer to the person reading', () => {
  it('says when it fires, in full', () => {
    timer('set_timer', { ...at, other_timers: [] })
    expect(
      screen.getByText('Fires Tuesday 29 September at 3:12 pm PDT (in 59 minutes)'),
    ).toBeInTheDocument()
    expect(screen.queryByText(/Also pending/)).not.toBeInTheDocument()
  })

  it('shows the timers it did not replace', () => {
    timer('set_timer', {
      ...at,
      other_timers: [{ id: 'a', when: 'Tuesday 29 September at 9:12 pm PDT', in: '7 hours', reason: 'food' }],
    })
    expect(screen.getByText(/Also pending/)).toBeInTheDocument()
    expect(screen.getByText(/9:12 pm PDT \(in 7 hours\)/)).toBeInTheDocument()
  })

  it('says when a sleep ends', () => {
    timer('sleep', { when: 'Tuesday 29 September at 2:14 pm PDT', in: '5 minutes' })
    expect(screen.getByText(/^Until Tuesday/)).toBeInTheDocument()
  })

  it('lists what is pending, or that nothing is', () => {
    timer('list_timers', { timers: [] })
    expect(screen.getByText('No timers pending')).toBeInTheDocument()
  })

  it('says which one was cancelled', () => {
    timer('cancel_timer', { cancelled: { when: 'Tuesday 29 September at 9:12 pm PDT', reason: 'food' } })
    expect(screen.getByText('Cancelled Tuesday 29 September at 9:12 pm PDT — food')).toBeInTheDocument()
  })

  it('falls back to the verb when there is nothing to read', () => {
    timer('set_timer', {})
    expect(screen.getByText('Setting a reminder')).toBeInTheDocument()
  })
})
