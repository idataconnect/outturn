import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import UsageRanked from './UsageRanked'

describe('UsageRanked', () => {
  it('shows a name where the key is an id, and the key where it is already a name', () => {
    render(
      <UsageRanked
        empty="nothing"
        slices={[
          { key: '01a0bc4d-5312-7851-96cd-be4a5d3c6481', label: 'Acme', calls: 2, tokens: 40 },
          { key: 'qwen3.5', label: null, calls: 1, tokens: 10 },
        ]}
      />,
    )
    expect(screen.getByText('Acme')).toBeInTheDocument()
    expect(screen.getByText('qwen3.5')).toBeInTheDocument()
    // The id is not what a reader knows the workspace by, so it is not shown.
    expect(screen.queryByText(/01a0bc4d/)).not.toBeInTheDocument()
  })

  it('says a null key is unattributed rather than inventing a category for it', () => {
    render(
      <UsageRanked empty="nothing" slices={[{ key: null, label: null, calls: 1, tokens: 10 }]} />,
    )
    expect(screen.getByText('Not attributed')).toBeInTheDocument()
  })

  it('lets a panel name its own null, since each dimension means a different thing by it', () => {
    // An agent panel's null is the platform naming a session -- work that ran
    // before there was an agent to bill it to, not spend that went astray.
    // Called "Not attributed" it read as a gap, which is what prompted this.
    render(
      <UsageRanked
        empty="nothing"
        unattributed="The platform itself"
        slices={[{ key: null, label: null, calls: 1, tokens: 10 }]}
      />,
    )
    expect(screen.getByText('The platform itself')).toBeInTheDocument()
    expect(screen.queryByText('Not attributed')).not.toBeInTheDocument()
  })

  it('shows each slice as a share of the window', () => {
    render(
      <UsageRanked
        empty="nothing"
        slices={[
          { key: 'a', label: null, calls: 1, tokens: 75 },
          { key: 'b', label: null, calls: 1, tokens: 25 },
        ]}
      />,
    )
    expect(screen.getByText('75%')).toBeInTheDocument()
    expect(screen.getByText('25%')).toBeInTheDocument()
  })

  it('falls back to its empty line rather than dividing by a zero total', () => {
    render(
      <UsageRanked
        empty="No model answered in this window."
        slices={[{ key: 'a', label: null, calls: 0, tokens: 0 }]}
      />,
    )
    expect(screen.getByText('No model answered in this window.')).toBeInTheDocument()
  })
})
