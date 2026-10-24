import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import ApprovalPrompt from './ApprovalPrompt'
import { answerApproval, markFollowUpDone, resendFollowUp } from '../lib/actions'

vi.mock('../lib/actions', () => ({
  answerApproval: vi.fn(async () => ({ resumed: 1 })),
  resendFollowUp: vi.fn(async () => ({ resumed: 1 })),
  markFollowUpDone: vi.fn(async () => ({ resumed: 0 })),
}))

const answer = vi.mocked(answerApproval)
const resend = vi.mocked(resendFollowUp)
const done = vi.mocked(markFollowUpDone)

const show = (
  approval: Parameters<typeof ApprovalPrompt>[0]['approval'] = {
    item_id: 'item-1',
    requires: 'charge',
    reason: 'POST /charges on the guesthouse needs approval',
  },
  onAnswered = vi.fn(),
) => {
  render(<ApprovalPrompt approval={approval} onAnswered={onAnswered} />)
  return { onAnswered }
}

/// A follow-up's yes sends the request again, so it is not worded or answered
/// as an approval.
describe('a follow-up', () => {
  beforeEach(() => {
    answer.mockClear()
    resend.mockClear()
    done.mockClear()
  })

  it('says an unknown outcome is not known', () => {
    show({ item_id: 'f-1', requires: 'charge', followup: 'unknown' })
    expect(
      screen.getByText(/sent but no answer came back, so whether it happened is not known/),
    ).toBeTruthy()
    expect(screen.queryByRole('button', { name: 'Approve' })).toBeNull()
    expect(screen.queryByRole('button', { name: 'Decline' })).toBeNull()
  })

  it('says an unsent one was approved but not sent', () => {
    show({ item_id: 'f-1', requires: 'charge', followup: 'unsent' })
    expect(screen.getByText('The charge was approved but could not be sent')).toBeTruthy()
  })

  it('sends again through resend, not answer', async () => {
    show({ item_id: 'f-1', requires: 'charge', followup: 'unknown' })
    await userEvent.click(screen.getByRole('button', { name: 'Send again' }))
    expect(resend).toHaveBeenCalledWith('f-1')
    expect(answer).not.toHaveBeenCalled()
  })

  it('marks as done through done', async () => {
    show({ item_id: 'f-1', requires: 'charge', followup: 'unsent' })
    await userEvent.click(screen.getByRole('button', { name: 'Mark as done' }))
    expect(done).toHaveBeenCalledWith('f-1')
    expect(answer).not.toHaveBeenCalled()
  })
})

describe('ApprovalPrompt', () => {
  beforeEach(() => answer.mockClear())

  it('says what is being asked and why', () => {
    show()
    expect(screen.getByText(/this needs approval: charge/i)).toBeTruthy()
    expect(screen.getByText(/POST \/charges on the guesthouse/i)).toBeTruthy()
  })

  /// The extent of a yes is what is shown beside it, and nothing wider.
  it('offers no wider extent, even where the request names a unit', () => {
    // Built apart so the extra field passes the type check, as it does when
    // it arrives from the server.
    const approval = {
      item_id: 'item-1',
      requires: 'charge',
      reason: 'a charge',
      covers: { field: 'booking_id', unit: 'bk_8812' },
    }
    show(approval)
    expect(screen.queryByRole('checkbox')).toBeNull()
  })

  it('approves the one request shown', async () => {
    show()
    await userEvent.click(screen.getByRole('button', { name: 'Approve' }))
    expect(answer).toHaveBeenCalledWith('item-1', { approved: true })
  })

  /// A decline settles the item and leaves the conversation paused, so it must
  /// not be sent as an approval with a flag the server has to interpret.
  it('declines as a decline', async () => {
    show()
    await userEvent.click(screen.getByRole('button', { name: 'Decline' }))
    expect(answer).toHaveBeenCalledWith('item-1', { approved: false })
  })

  it('tells the caller once it is answered', async () => {
    const { onAnswered } = show()
    await userEvent.click(screen.getByRole('button', { name: 'Approve' }))
    expect(onAnswered).toHaveBeenCalled()
  })

  /// The turn is still held when this fails, so a button that appeared to do
  /// nothing is how somebody concludes the conversation is broken.
  it('leaves the failure on screen and lets the person try again', async () => {
    answer.mockRejectedValueOnce(new Error('the network went away'))
    const { onAnswered } = show()
    await userEvent.click(screen.getByRole('button', { name: 'Approve' }))

    expect(await screen.findByRole('alert')).toHaveTextContent(/network went away/i)
    expect(onAnswered).not.toHaveBeenCalled()
    // Not left disabled: the request failed, so the person has to be able to
    // send it again.
    expect((screen.getByRole('button', { name: 'Approve' }) as HTMLButtonElement).disabled).toBe(
      false,
    )
  })
})

describe('what an approval is for', () => {
  /// The reason a person used to be asked to approve a charge without seeing
  /// its amount: the card said which path, never which values.
  it('shows each value being approved, exactly as the request has it', () => {
    show({
      item_id: 'item-1',
      requires: 'charge',
      reason: 'a charge',
      binds: [
        { field: 'booking_id', value: 'bk_8812' },
        { field: 'amount_pence', value: 78000 },
      ],
    })
    expect(screen.getByText('booking_id')).toBeTruthy()
    expect(screen.getByText('bk_8812')).toBeTruthy()
    expect(screen.getByText('amount_pence')).toBeTruthy()
    // Not "£780", not "78,000": what the grant is keyed on.
    expect(screen.getByText('78000')).toBeTruthy()
  })

  it('says a field the request should carry is missing', () => {
    show({
      item_id: 'item-1',
      requires: 'charge',
      reason: 'a charge',
      binds: [{ field: 'payment_account_id', value: null }],
    })
    expect(screen.getByText('missing')).toBeTruthy()
  })
})
