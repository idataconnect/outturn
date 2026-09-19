import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import StopDialog from './StopDialog'

const show = (onStop = vi.fn(async () => {}), onClose = vi.fn()) => {
  render(<StopDialog subject="this whole workspace" onStop={onStop} onClose={onClose} />)
  return { onStop, onClose }
}

describe('StopDialog', () => {
  it('is announced as a dialog and starts in the reason', () => {
    show()
    expect(screen.getByRole('dialog', { name: /stop this whole workspace/i })).toBeTruthy()
    expect(document.activeElement).toBe(screen.getByLabelText('Reason'))
  })

  it('will not stop without a reason', async () => {
    const { onStop } = show()
    const button = screen.getByRole('button', { name: 'Stop' }) as HTMLButtonElement
    expect(button.disabled).toBe(true)
    await userEvent.type(screen.getByLabelText('Reason'), '   ')
    expect(button.disabled).toBe(true)
    expect(onStop).not.toHaveBeenCalled()
  })

  it('stops with the trimmed reason, then closes', async () => {
    const { onStop, onClose } = show()
    await userEvent.type(screen.getByLabelText('Reason'), '  runaway spend ')
    await userEvent.click(screen.getByRole('button', { name: 'Stop' }))
    expect(onStop).toHaveBeenCalledWith('runaway spend')
    expect(onClose).toHaveBeenCalled()
  })

  it('keeps the reason and shows the failure when stopping fails', async () => {
    const { onClose } = show(vi.fn(async () => { throw new Error('boom') }))
    await userEvent.type(screen.getByLabelText('Reason'), 'runaway spend')
    await userEvent.click(screen.getByRole('button', { name: 'Stop' }))
    expect(screen.getByRole('alert').textContent).toBe('failed to stop')
    expect((screen.getByLabelText('Reason') as HTMLTextAreaElement).value).toBe('runaway spend')
    expect(onClose).not.toHaveBeenCalled()
  })

  it('closes on Escape', async () => {
    const { onClose } = show()
    await userEvent.keyboard('{Escape}')
    expect(onClose).toHaveBeenCalled()
  })
})
