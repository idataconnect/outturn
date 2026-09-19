import type { ToolCallMessagePartProps } from '@assistant-ui/react'
import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import ToolClock from './ToolClock'

type ClockArgs = {
  action?: string
  details?: string
  isError?: boolean
  pending?: boolean
}

function clock(args: ClockArgs) {
  // Only `toolName` and `args` are read, here or in `ToolCall`. The rest of a
  // message part -- the status machinery, the result sink, the ids the runtime
  // threads through -- belongs to whatever is driving the thread, and building
  // a convincing one would test the library rather than this component. So the
  // two fields that matter are given honestly and the cast covers the rest.
  const props = { toolName: 'get_current_time', args } as unknown as ToolCallMessagePartProps
  return render(<ToolClock {...props} />)
}

function details(over: Record<string, unknown> = {}) {
  return JSON.stringify({
    now: '2026-09-19T14:14:00+10:00',
    weekday: 'Saturday',
    timezone: 'Australia/Brisbane',
    abbreviation: 'AEST',
    ...over,
  })
}

describe('showing what the clock answered', () => {
  it('reads the date and time as a sentence', () => {
    clock({ action: "Checking today's date", details: details() })
    expect(screen.getByText('Saturday, 19 September 2026 at 2:14 pm AEST')).toBeInTheDocument()
  })

  it('shows the time in the turn owner is zone, not the reader is', () => {
    // The whole reason the clock is a host import: the zone belongs to the
    // user the turn is for. A renderer that parsed this into a Date would
    // print it in whatever zone the browser sits in, and silently contradict
    // what the agent was told. 14:14+10:00 is 04:14 UTC -- if this ever reads
    // "4:14 am", the renderer has started reformatting.
    clock({ details: details() })
    expect(screen.getByText(/2:14 pm AEST/)).toBeInTheDocument()
    expect(screen.queryByText(/4:14/)).not.toBeInTheDocument()
  })

  it('reads midnight and noon as 12, not 0', () => {
    clock({ details: details({ now: '2026-09-19T00:05:00+10:00' }) })
    expect(screen.getByText(/12:05 am/)).toBeInTheDocument()
  })

  it('keeps the zone off when the host did not name one', () => {
    // An empty abbreviation means the zone was unknown and the clock read
    // UTC. Inventing a label for it would assert something nobody said.
    clock({ details: details({ abbreviation: '' }) })
    expect(screen.getByText('Saturday, 19 September 2026 at 2:14 pm')).toBeInTheDocument()
  })

  it('shows the verb alone when there is nothing to read', () => {
    clock({ action: 'Checking the time', details: '' })
    expect(screen.getByText('Checking the time')).toBeInTheDocument()
  })

  it('does not fall over on a result that is not JSON', () => {
    clock({ action: 'Checking the time', details: 'upstream said no' })
    expect(screen.getByText('Checking the time')).toBeInTheDocument()
  })
})
