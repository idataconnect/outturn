import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import SkillStats, { type SkillStats as Stats } from './SkillStats'

const base: Stats = {
  from: '2026-08-30T00:00:00Z',
  to: '2026-09-29T00:00:00Z',
  totals: { skills: 3, bound: 2, versions: 4, created: 1, retired: 0, turns: 20 },
  used: [],
  idle: [],
  lagging: [],
  authors: [],
}

const shown = (over: Partial<Stats>) => render(<SkillStats stats={{ ...base, ...over }} />)

describe('the skills panel', () => {
  it('says what the headline numbers are of', () => {
    shown({})
    expect(screen.getByText('2 carried by an agent')).toBeInTheDocument()
    expect(screen.getByText('1 new, 0 retired')).toBeInTheDocument()
  })

  /// Conversations as well as turns: one busy session and twenty separate ones
  /// are different facts about how much a skill is relied on.
  it('separates turns from the conversations they happened in', () => {
    shown({
      used: [
        {
          skill_id: 's1',
          name: 'Booking',
          slug: 'booking',
          turns: 20,
          sessions: 10,
          versions: 0,
          last_used: new Date().toISOString(),
        },
      ],
    })
    expect(screen.getByText(/20 turns/)).toBeInTheDocument()
    expect(screen.getByText(/10 conversations/)).toBeInTheDocument()
  })

  /// The panel's whole purpose: a pin is somebody's decision, its absence is an
  /// edit that never reached the agent. Saying "v1 of 2" without that leaves
  /// the reader unable to tell which.
  it('says whether an old version is deliberate', () => {
    shown({
      lagging: [
        {
          skill_id: 's1',
          name: 'Charging',
          slug: 'charging',
          latest: 2,
          serving: 1,
          turns: 5,
          pinned: false,
        },
      ],
    })
    expect(screen.getByText(/may not have reached the agent/)).toBeInTheDocument()
  })

  it('and says so when it is', () => {
    shown({
      lagging: [
        {
          skill_id: 's1',
          name: 'Charging',
          slug: 'charging',
          latest: 2,
          serving: 1,
          turns: 5,
          pinned: true,
        },
      ],
    })
    expect(screen.getByText(/deliberate/)).toBeInTheDocument()
  })

  /// "Never" is a stronger statement than "not lately", and the API reaches
  /// outside the window to be able to make it.
  it('distinguishes a skill that has never been used from a stale one', () => {
    shown({
      idle: [
        { skill_id: 's1', name: 'Refunds', slug: 'refunds', agents: 1, last_used: null },
        {
          skill_id: 's2',
          name: 'Upsell',
          slug: 'upsell',
          agents: 2,
          last_used: new Date(Date.now() - 86_400_000 * 60).toISOString(),
        },
      ],
    })
    expect(screen.getByText(/last used never/)).toBeInTheDocument()
    expect(screen.getByText(/2 months ago/)).toBeInTheDocument()
  })

  /// A version written by an install carries no author. Named rather than left
  /// blank, so a row with no name does not read as a rendering fault.
  it('names the platform where there is no author', () => {
    shown({ authors: [{ user_id: null, name: null, versions: 2, skills: 1 }] })
    expect(screen.getByText('the platform')).toBeInTheDocument()
  })

  /// The quiet panels stay quiet. A workspace with nothing wrong should see no
  /// warnings at all.
  it('shows no warnings when there is nothing to warn about', () => {
    shown({})
    expect(screen.queryByText(/Running an older version/)).not.toBeInTheDocument()
    // The tile stays -- "0 idle" is worth saying. What must not appear is the
    // list panel, which exists only to name the offenders.
    expect(screen.queryByText(/Carried, and used by nothing/)).not.toBeInTheDocument()
    expect(screen.getByText('Idle')).toBeInTheDocument()
  })

  it('says plainly when the workspace has no skills', () => {
    shown({ totals: { ...base.totals, skills: 0 } })
    expect(screen.getByText(/no skills yet/)).toBeInTheDocument()
  })
})
