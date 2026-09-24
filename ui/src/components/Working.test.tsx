import { act, render } from '@testing-library/react'
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

    // Six per dot: a rect swells by moving four sides and its corner radius,
    // where a circle needed only `r`. The shape is a rect throughout so that
    // widening it into the line is one animation rather than a swap.
    expect(container.querySelectorAll('animate')).toHaveLength(18)
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

    expect(container.querySelectorAll('rect')).toHaveLength(3)
  })

  it('staggers each dot\'s colour to match its place in the swell', () => {
    prefersReducedMotion(false)

    const { container } = render(<Working />)

    const delays = Array.from(container.querySelectorAll('rect')).map(
      (dot) => dot.style.animationDelay,
    )
    // The same 0.4s stagger the swell uses, so the dot wearing the accent is
    // the dot that is widest rather than one trailing behind it.
    expect(delays).toEqual(['0s', '0.4s', '0.8s'])
  })

  it('leaves the colour to the stylesheet, so a theme can replace it', () => {
    prefersReducedMotion(false)

    const { container } = render(<Working />)

    for (const dot of container.querySelectorAll('rect')) {
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

describe('the mark when the turn ends', () => {
  it('draws its dots together into a line', async () => {
    prefersReducedMotion(false)

    const { container, rerender } = render(<Working />)
    expect(container.querySelectorAll('rect')[0].className.baseVal).not.toContain(
      'settling',
    )

    rerender(<Working phase="done" />)

    // The same three shapes, handed to a CSS animation that owns their
    // geometry for its duration. Not SMIL, which is what the swell uses: a
    // second SMIL animation begins from the element's base attributes rather
    // than from the value the first one froze, so the dot snapped back to its
    // resting size before moving however the curves were tuned.
    const shapes = container.querySelectorAll('rect')
    expect(shapes).toHaveLength(3)
    for (const shape of shapes) {
      expect(shape.className.baseVal).toContain('working-dot-settling')
    }
    // The swell's SMIL is gone, so nothing is left fighting the keyframes.
    expect(container.querySelectorAll('animate')).toHaveLength(0)
  })

  it('is left holding a line rather than disappearing', async () => {
    // A mark that vanished would leave a finished reply looking like one
    // still being written, which is the distinction it exists to draw.
    prefersReducedMotion(false)
    vi.useFakeTimers()
    try {
      const { container, rerender } = render(<Working />)
      rerender(<Working phase="done" />)
      await act(async () => {
        vi.advanceTimersByTime(1000)
      })
      // Three shapes still there, still wearing the animation that ends on
      // the line and holds it -- `forwards`, so what it computed last is what
      // stays. The attributes keep their resting values; the keyframe owns
      // them.
      const shapes = container.querySelectorAll('rect')
      expect(shapes).toHaveLength(3)
      for (const shape of shapes) {
        expect(shape.className.baseVal).toContain('working-dot-settling')
      }
    } finally {
      vi.useRealTimers()
    }
  })

  it('says which state it is in, for somebody who cannot see it', () => {
    prefersReducedMotion(false)
    const { getByRole, rerender } = render(<Working />)
    expect(getByRole('status')).toHaveAttribute('aria-label', 'Working')

    rerender(<Working phase="done" />)
    expect(getByRole('status')).toHaveAttribute('aria-label', 'Finished')
  })

  it('goes straight to the line when less motion was asked for', () => {
    // No travel to watch, so there is nothing to animate into: the line is
    // simply what is there once the turn is over.
    prefersReducedMotion(true)

    const { container, rerender } = render(<Working />)
    rerender(<Working phase="done" />)

    expect(container.querySelectorAll('animate')).toHaveLength(0)
    expect(container.querySelectorAll('rect')[0].className.baseVal).toContain(
      'working-dot-settling',
    )
  })

  it('breathes together while held, rather than passing a swell along', () => {
    prefersReducedMotion(false)

    const { container } = render(<Working phase="held" />)

    // The travelling swell is SMIL and belongs to `running` alone. Held is a
    // CSS pulse: no <animate> at all, which is also what stops the two from
    // running at once on the same attributes.
    expect(container.querySelectorAll('animate')).toHaveLength(0)
    for (const dot of container.querySelectorAll('rect')) {
      expect(dot.className.baseVal).toContain('working-dot-held')
    }
  })

  it('gives the held dots no stagger, so they pulse as one object', () => {
    prefersReducedMotion(false)

    const { container } = render(<Working phase="held" />)

    // The whole distinction the mark rests on: in unison reads as waiting,
    // travelling reads as advancing. A stagger here would blur the two.
    const delays = Array.from(container.querySelectorAll('rect')).map(
      (dot) => dot.style.animationDelay,
    )
    expect(delays).toEqual(['0s', '0s', '0s'])
  })

  it('says what a held turn is waiting for, not just that it is waiting', () => {
    prefersReducedMotion(false)

    const { getByRole } = render(
      <Working phase="held" label="Waiting for the model" />,
    )

    // `waiting` and `retrying` move the same way, because they are the same
    // news. What separates them is only ever said in words.
    expect(getByRole('status')).toHaveAttribute('aria-label', 'Waiting for the model')
  })

  it('draws no pulse for somebody who asked for less motion', () => {
    prefersReducedMotion(true)

    const { container } = render(<Working phase="held" />)

    // The class carries a CSS animation, so unlike the SMIL case the element
    // may exist -- but the stylesheet must not animate it. Asserted on the
    // class the component chooses, since jsdom applies no stylesheet.
    for (const dot of container.querySelectorAll('rect')) {
      expect(dot.className.baseVal).not.toContain('working-dot-held')
    }
  })
})
