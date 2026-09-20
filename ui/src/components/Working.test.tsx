import { render } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import Working from './Working'

function prefersReducedMotion(reduce: boolean) {
  vi.stubGlobal('matchMedia', (query: string) => ({
    matches: query.includes('prefers-reduced-motion') ? reduce : false,
    addEventListener: () => {},
    removeEventListener: () => {},
  }))
}

describe('the working mark', () => {
  it('animates its dots by default', () => {
    prefersReducedMotion(false)

    const { container } = render(<Working />)

    // Two per dot: one for the swell, one for the brightness.
    expect(container.querySelectorAll('animate')).toHaveLength(6)
  })

  it('draws no animation at all for somebody who asked for less motion', () => {
    prefersReducedMotion(true)

    const { container } = render(<Working />)

    // Not merely paused: SMIL ignores prefers-reduced-motion, so the only way
    // to honour it is for the elements not to exist.
    expect(container.querySelectorAll('animate')).toHaveLength(0)
  })

  it('still shows the dots when motion is unwanted, because it still has news', () => {
    prefersReducedMotion(true)

    const { container } = render(<Working />)

    expect(container.querySelectorAll('circle')).toHaveLength(3)
  })

  it('staggers each dot\'s colour to match its place in the swell', () => {
    prefersReducedMotion(false)

    const { container } = render(<Working />)

    const delays = Array.from(container.querySelectorAll('circle')).map(
      (dot) => (dot as SVGCircleElement).style.animationDelay,
    )
    // The same 0.4s stagger the swell uses, so the dot wearing the accent is
    // the dot that is widest rather than one trailing behind it.
    expect(delays).toEqual(['0s', '0.4s', '0.8s'])
  })

  it('leaves the colour to the stylesheet, so a theme can replace it', () => {
    prefersReducedMotion(false)

    const { container } = render(<Working />)

    for (const dot of container.querySelectorAll('circle')) {
      // A fill resolved in the component would stop following a theme that
      // replaced the token.
      expect(dot.getAttribute('fill')).toBeNull()
      expect(dot).toHaveClass('working-dot')
    }
  })

  it('names itself for a reader who cannot see it', () => {
    prefersReducedMotion(false)

    const { getByRole } = render(<Working />)

    expect(getByRole('status', { name: 'Working' })).toBeInTheDocument()
  })
})
