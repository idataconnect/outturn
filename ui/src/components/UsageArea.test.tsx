import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import UsageArea, { type Bucket } from './UsageArea'

function bucket(at: string, over: Partial<Bucket> = {}): Bucket {
  return {
    at,
    calls: 0,
    prompt_tokens: 0,
    completion_tokens: 0,
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    reasoning_tokens: 0,
    ...over,
  }
}

describe('UsageArea', () => {
  it('draws a band per kind the window actually contains, and no others', () => {
    const { container } = render(
      <UsageArea
        buckets={[
          bucket('2026-09-18T00:00:00Z', { calls: 2, prompt_tokens: 100, completion_tokens: 20 }),
          bucket('2026-09-19T00:00:00Z', { calls: 3, prompt_tokens: 150, completion_tokens: 30 }),
        ]}
      />,
    )

    // Two kinds are present, so two legend entries and no more. A band that is
    // zero everywhere must not appear: a legend listing "Cache write" for a
    // provider that never reported one teaches the reader something untrue.
    expect(screen.getByText('Prompt')).toBeInTheDocument()
    expect(screen.getByText('Completion')).toBeInTheDocument()
    expect(screen.queryByText('Cache write')).not.toBeInTheDocument()
    expect(screen.queryByText('Reasoning')).not.toBeInTheDocument()

    // One filled wash and one 2px top line per band.
    expect(container.querySelectorAll('path[fill-opacity]')).toHaveLength(2)
  })

  it('says so rather than drawing an empty stack when nothing happened', () => {
    render(
      <UsageArea
        buckets={[bucket('2026-09-18T00:00:00Z'), bucket('2026-09-19T00:00:00Z')]}
      />,
    )
    expect(screen.getByText('No model calls in this window.')).toBeInTheDocument()
  })

  it('keeps a single bucket on the chart rather than collapsing it onto the edge', () => {
    const { container } = render(
      <UsageArea buckets={[bucket('2026-09-19T00:00:00Z', { calls: 1, prompt_tokens: 10 })]} />,
    )
    // A lone point has no width to interpolate across; the guard against
    // dividing by zero is what keeps its coordinates finite.
    const path = container.querySelector('path[fill-opacity]')?.getAttribute('d') ?? ''
    expect(path).not.toContain('NaN')
    expect(path.length).toBeGreaterThan(0)
  })

  it('keeps cache reads out of the stack, on their own stated scale', () => {
    const { container } = render(
      <UsageArea
        buckets={[
          // The shape a real transcript-heavy window has: cache reads an order
          // of magnitude above the work. Stacked, they would leave completion
          // tokens a few pixels tall.
          bucket('2026-09-19T00:00:00Z', {
            calls: 236,
            prompt_tokens: 85960,
            completion_tokens: 29802,
            cache_read_tokens: 873599,
          }),
          bucket('2026-09-20T00:00:00Z', {
            calls: 18,
            prompt_tokens: 12560,
            completion_tokens: 3569,
            cache_read_tokens: 25013,
          }),
        ]}
      />,
    )

    // Drawn dashed, because it is the one mark that does not share the axis
    // beside it, and the legend says so in words.
    expect(container.querySelector('path[stroke-dasharray]')).toBeTruthy()
    expect(screen.getByText(/cache read — own scale/i)).toBeInTheDocument()

    // The stack keeps its own maximum, so the work is still legible: were the
    // cache reads in it, the axis would top out near a million and these bands
    // would be a sliver.
    const stacked = [...container.querySelectorAll('path[fill-opacity]')]
    expect(stacked.length).toBe(2)
    const spans = stacked.map((path) => {
      const ys = (path.getAttribute('d') ?? '')
        .split(/[ML] /)
        .filter(Boolean)
        .map((p) => Number(p.trim().split(',')[1]))
        .filter((n) => !Number.isNaN(n))
      return Math.max(...ys) - Math.min(...ys)
    })
    for (const span of spans) {
      expect(span).toBeGreaterThan(20)
    }
  })

  it('draws a window that is nothing but cache reads rather than calling it empty', () => {
    render(
      <UsageArea buckets={[bucket('2026-09-19T00:00:00Z', { calls: 1, cache_read_tokens: 500 })]} />,
    )
    expect(screen.queryByText('No model calls in this window.')).not.toBeInTheDocument()
  })

  it('describes itself for a reader who cannot see it', () => {
    render(<UsageArea buckets={[bucket('2026-09-19T00:00:00Z', { prompt_tokens: 5 })]} />)
    expect(screen.getByRole('img')).toHaveAccessibleName(/tokens per day/i)
  })
})
