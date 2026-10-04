import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import type { ToolCallMessagePartProps } from '@assistant-ui/react'

import ToolTarget from './ToolTarget'
import { httpFailure } from '../lib/httpFailure'

const fetchCall = (status: number) =>
  ({
    toolName: 'fetch_url',
    toolCallId: 't',
    args: {
      action: 'Fetching the items',
      details: JSON.stringify({
        method: 'GET',
        url: 'https://books.example.com/api/items',
        status,
      }),
    },
  }) as unknown as ToolCallMessagePartProps

describe('a fetch the server refused', () => {
  /// The request went out and came back, so the tool did not error -- but a
  /// green tick beside a 401 tells the reader the call worked.
  it('shows as a failure, with the status in words', () => {
    render(<ToolTarget {...fetchCall(401)} />)
    expect(screen.queryByRole('img', { name: 'Done' })).not.toBeInTheDocument()
    expect(screen.getByText('The server answered 401 Unauthorized.')).toBeInTheDocument()
  })

  it('a successful one still shows as done', () => {
    render(<ToolTarget {...fetchCall(200)} />)
    expect(screen.getByRole('img', { name: 'Done' })).toBeInTheDocument()
  })

  it('reads an unknown error status without a reason phrase', () => {
    expect(httpFailure(JSON.stringify({ status: 418 }))).toBe('The server answered 418.')
    expect(httpFailure(JSON.stringify({ status: 302 }))).toBeUndefined()
    expect(httpFailure('not json')).toBeUndefined()
  })
})
