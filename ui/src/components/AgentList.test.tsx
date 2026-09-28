import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter } from 'react-router'
import { describe, expect, it } from 'vitest'

import AgentList from './AgentList'
import type { Agent } from '../lib/chat'

const agent = (over: Partial<Agent> & Pick<Agent, 'id' | 'name'>): Agent => ({
  slug: over.name.toLowerCase().replace(/\s+/g, '-'),
  description: '',
  enabled: true,
  can_chat: true,
  ...over,
})

const roster = [
  agent({ id: 'a1', name: 'Front desk', description: 'Bookings and payments' }),
  agent({ id: 'a2', name: 'Accounts receivable', slug: 'ar' }),
]

function show(agents: Agent[], current?: string) {
  return render(
    <MemoryRouter>
      <AgentList
        agents={agents}
        href={(a) => `/agents/${a.id}`}
        current={current}
        empty={<p>Nobody here</p>}
      />
    </MemoryRouter>,
  )
}

describe('the agent list', () => {
  it('finds an agent by what it is for, not only by name', async () => {
    const user = userEvent.setup()
    show(roster)

    await user.type(screen.getByRole('searchbox'), 'payments')

    expect(screen.getByRole('link', { name: /front desk/i })).toBeInTheDocument()
    expect(screen.queryByRole('link', { name: /accounts/i })).toBeNull()
  })

  it('finds one by slug', async () => {
    const user = userEvent.setup()
    show(roster)

    await user.type(screen.getByRole('searchbox'), 'ar')

    expect(screen.getByRole('link', { name: /accounts/i })).toBeInTheDocument()
  })

  it('says nothing matched, which is not the same as there being nobody', async () => {
    const user = userEvent.setup()
    show(roster)

    await user.type(screen.getByRole('searchbox'), 'zzz')

    expect(screen.getByText(/no agent matches/i)).toBeInTheDocument()
    expect(screen.queryByText('Nobody here')).toBeNull()
  })

  it('says there is nobody, with no filter to type into', () => {
    show([])

    expect(screen.getByText('Nobody here')).toBeInTheDocument()
    expect(screen.queryByRole('searchbox')).toBeNull()
  })

  it('marks the open agent, and only that one', () => {
    show(roster, 'a2')

    expect(screen.getByRole('link', { name: /accounts/i })).toHaveAttribute('aria-current', 'page')
    expect(screen.getByRole('link', { name: /front desk/i })).not.toHaveAttribute('aria-current')
  })
})
