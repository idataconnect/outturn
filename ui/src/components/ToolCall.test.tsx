import type { ToolCallMessagePartProps } from '@assistant-ui/react'
import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import ToolCall from './ToolCall'

function call(args: Record<string, unknown>) {
  const props = { toolName: 'fetch_url', args } as unknown as ToolCallMessagePartProps
  return render(<ToolCall {...props} />)
}

describe('a call held for approval', () => {
  it('reads as waiting, not as an error', () => {
    // Held is reported as an error to the model, which must not retry it,
    // but to a person it is a question somebody has not answered yet.
    const { container } = call({
      action: 'Charging the card',
      details: 'this needs approval before it can go out',
      isError: true,
      gated: true,
    })
    expect(screen.getByRole('img', { name: 'Waiting for approval' })).toBeInTheDocument()
    expect(screen.getByText('this needs approval before it can go out')).toBeInTheDocument()
    expect(container.firstElementChild).toHaveAttribute('data-state', 'gated')
  })

  it('still reads as an error when it really failed', () => {
    const { container } = call({ details: 'connection refused', isError: true })
    expect(screen.queryByRole('img', { name: 'Waiting for approval' })).toBeNull()
    expect(container.firstElementChild).toHaveAttribute('data-state', 'error')
  })
})
