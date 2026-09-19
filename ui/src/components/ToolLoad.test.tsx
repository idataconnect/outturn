import type { ToolCallMessagePartProps } from '@assistant-ui/react'
import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import ToolLoad from './ToolLoad'

type LoadArgs = {
  action?: string
  details?: string
  isError?: boolean
  pending?: boolean
}

function load(args: LoadArgs) {
  // Only `toolName` and `args` are read, here or in `ToolCall`. The rest of a
  // message part belongs to whatever drives the thread, and building a
  // convincing one would test the library rather than this component.
  const props = { toolName: 'load_tools', args } as unknown as ToolCallMessagePartProps
  return render(<ToolLoad {...props} />)
}

describe('showing what a load made available', () => {
  it('lists the tools, comma separated', () => {
    load({
      action: 'Getting the tools ready',
      details: JSON.stringify({ loaded: ['fetch_url', 'read_object'] }),
    })
    expect(screen.getByText('fetch_url, read_object')).toBeInTheDocument()
  })

  it('lists what did load when only some names were recognised', () => {
    // A partial load is still a load: the turn continues with what arrived,
    // so the reader is told what that was.
    load({
      details: JSON.stringify({
        loaded: ['fetch_url'],
        no_such_tool: ['fetch_the_web'],
      }),
    })
    expect(screen.getByText('fetch_url')).toBeInTheDocument()
  })

  it('shows the failure and no list when nothing was recognised', () => {
    // Nothing loaded, so there is no list to show -- and the error is what
    // the reader needs. An empty list beside a warning says less than the
    // warning alone.
    load({
      action: 'Getting the tools ready',
      isError: true,
      details: JSON.stringify({
        error: 'no such tool: fetch_the_web. Available: fetch_url',
        no_such_tool: ['fetch_the_web'],
      }),
    })
    expect(
      screen.getByText('no such tool: fetch_the_web. Available: fetch_url'),
    ).toBeInTheDocument()
    expect(screen.getByText('Getting the tools ready')).toBeInTheDocument()
  })

  it('shows the verb alone while the call is still running', () => {
    load({ action: 'Getting the tools ready', pending: true, details: '' })
    expect(screen.getByText('Getting the tools ready')).toBeInTheDocument()
  })

  it('does not fall over on a result that is not JSON', () => {
    load({ action: 'Getting the tools ready', details: 'upstream said no' })
    expect(screen.getByText('Getting the tools ready')).toBeInTheDocument()
  })
})
