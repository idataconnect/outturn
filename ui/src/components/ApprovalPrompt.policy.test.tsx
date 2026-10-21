import { render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import ApprovalPrompt from './ApprovalPrompt'
import { answeredCount } from '../lib/autoApproval'
import { SessionContext, type SessionState } from '../lib/session'

vi.mock('../lib/actions', () => ({ answerApproval: vi.fn() }))
vi.mock('../lib/autoApproval', async (actual) => ({
  ...(await actual<typeof import('../lib/autoApproval')>()),
  answeredCount: vi.fn(),
}))

const counted = vi.mocked(answeredCount)

function signedIn(authorities: string[]): SessionState {
  return {
    status: 'authenticated',
    displayName: 'Tester',
    workspaces: [],
    session: {
      session_id: 's',
      workspace_id: 'w',
      display_name: 'Tester',
      roles: [],
      authorities,
      workspaces: [],
    },
  }
}

const show = (authorities: string[]) =>
  render(
    <SessionContext.Provider value={signedIn(authorities)}>
      <ApprovalPrompt
        approval={{
          item_id: 'item-1',
          requires: 'charge',
          binds: [
            { field: 'booking_id', value: 'bk_1' },
            { field: 'amount_pence', value: 4000 },
          ],
        }}
        onAnswered={vi.fn()}
      />
    </SessionContext.Provider>,
  )

describe('ApprovalPrompt and auto-approval', () => {
  beforeEach(() => counted.mockReset())

  it('links to a prefilled policy once the act has been approved three times', async () => {
    counted.mockResolvedValue({ requires: 'charge', approved: 3, may_delegate: true })
    show(['approvals:answer', 'approvals:delegate'])
    const link = await screen.findByRole('link', { name: /auto-approval policy for “charge”/i })
    const href = link.getAttribute('href') ?? ''
    expect(href).toContain('/settings/auto-approval/new?')
    const query = new URLSearchParams(href.split('?')[1])
    expect(query.get('requires')).toBe('charge')
    expect(query.get('selector')).toBe('act')
    expect(query.getAll('field')).toEqual(['booking_id', 'amount_pence'])
    // A link, and never a box ticked on the way past.
    expect(screen.queryByRole('checkbox')).toBeNull()
  })

  it('offers nothing below three', async () => {
    counted.mockResolvedValue({ requires: 'charge', approved: 2, may_delegate: true })
    show(['approvals:answer', 'approvals:delegate'])
    await waitFor(() => expect(counted).toHaveBeenCalled())
    expect(screen.queryByRole('link')).toBeNull()
  })

  it('does not ask, or offer, for somebody who could not set a policy', () => {
    show(['approvals:answer'])
    expect(counted).not.toHaveBeenCalled()
    expect(screen.queryByRole('link')).toBeNull()
  })
})
