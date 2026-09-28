import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import ApprovalPrompt from './ApprovalPrompt'
import { answerApproval } from '../lib/actions'

vi.mock('../lib/actions', () => ({ answerApproval: vi.fn(async () => ({ resumed: 1 })) }))

const answer = vi.mocked(answerApproval)

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

describe('ApprovalPrompt', () => {
  beforeEach(() => answer.mockClear())

  it('says what is being asked and why', () => {
    show()
    expect(screen.getByText(/this needs approval: charge/i)).toBeTruthy()
    expect(screen.getByText(/POST \/charges on the guesthouse/i)).toBeTruthy()
  })

  /// The offer is not the grant. A default that widens the extent is a default
  /// that answers for the person, which is the permission-dialog failure the
  /// whole design is arranged against.
  it('never pre-ticks the wider extent', () => {
    show({
      item_id: 'item-1',
      requires: 'charge',
      reason: 'a charge',
      covers: { field: 'booking_id', unit: 'bk_8812' },
    })
    const tick = screen.getByRole('checkbox') as HTMLInputElement
    expect(tick.checked).toBe(false)
  })

  it('offers no tickbox where the gate declared no unit', () => {
    show()
    expect(screen.queryByRole('checkbox')).toBeNull()
  })

  /// Approving without ticking grants the narrow extent, whatever was offered.
  it('approves the one request unless the extent was ticked', async () => {
    show({
      item_id: 'item-1',
      requires: 'charge',
      reason: 'a charge',
      covers: { field: 'booking_id', unit: 'bk_8812' },
    })
    await userEvent.click(screen.getByRole('button', { name: 'Approve' }))
    expect(answer).toHaveBeenCalledWith('item-1', {
      approved: true,
      coversUnit: false,
    })
  })

  it('widens the extent only when the person ticked it', async () => {
    show({
      item_id: 'item-1',
      requires: 'charge',
      reason: 'a charge',
      covers: { field: 'booking_id', unit: 'bk_8812' },
    })
    await userEvent.click(screen.getByRole('checkbox'))
    await userEvent.click(screen.getByRole('button', { name: 'Approve' }))
    expect(answer).toHaveBeenCalledWith('item-1', {
      approved: true,
      coversUnit: true,
    })
  })

  /// A decline settles the item and leaves the conversation paused, so it must
  /// not be sent as an approval with a flag the server has to interpret.
  it('declines as a decline', async () => {
    show()
    await userEvent.click(screen.getByRole('button', { name: 'Decline' }))
    expect(answer).toHaveBeenCalledWith('item-1', {
      approved: false,
      coversUnit: false,
    })
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
