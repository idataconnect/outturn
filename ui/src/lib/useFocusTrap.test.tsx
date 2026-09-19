import { describe, expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useRef } from 'react'

import { useFocusTrap } from './useFocusTrap'

/**
 * A dialog with one of every control a trap has to catch.
 *
 * The link and the textarea are the point: the two hand-written traps this
 * hook replaced each omitted one of them, and each looked correct until a
 * dialog happened to contain the control its selector missed.
 */
function Dialog({ onClose }: { onClose: () => void }) {
  const dialog = useRef<HTMLDivElement>(null)
  useFocusTrap(dialog, onClose)
  return (
    <div ref={dialog}>
      <a href="#first">first</a>
      <textarea aria-label="reason" />
      <button>last</button>
    </div>
  )
}

describe('keeping focus inside a dialog', () => {
  it('starts on the first focusable thing', () => {
    render(<Dialog onClose={() => {}} />)
    expect(document.activeElement).toBe(screen.getByRole('link'))
  })

  it('closes on Escape', async () => {
    const onClose = vi.fn()
    render(<Dialog onClose={onClose} />)
    await userEvent.keyboard('{Escape}')
    expect(onClose).toHaveBeenCalledOnce()
  })

  it('wraps from the last control back to the first', async () => {
    render(<Dialog onClose={() => {}} />)
    screen.getByRole('button').focus()
    await userEvent.tab()
    expect(document.activeElement).toBe(screen.getByRole('link'))
  })

  it('wraps backwards from the first to the last', async () => {
    render(<Dialog onClose={() => {}} />)
    screen.getByRole('link').focus()
    await userEvent.tab({ shift: true })
    expect(document.activeElement).toBe(screen.getByRole('button'))
  })

  // The drift that prompted the extraction: one trap caught links but not
  // textareas, the other the reverse, so a Tab through the missing kind left
  // the dialog entirely.
  it('counts links and textareas alike, so neither is tabbed past', async () => {
    render(<Dialog onClose={() => {}} />)
    screen.getByRole('link').focus()
    await userEvent.tab()
    expect(document.activeElement).toBe(screen.getByLabelText('reason'))
    await userEvent.tab()
    expect(document.activeElement).toBe(screen.getByRole('button'))
  })

  it('puts focus back where it came from on close', () => {
    render(
      <>
        <button>opener</button>
        <div id="host" />
      </>,
    )
    const opener = screen.getByRole('button', { name: 'opener' })
    opener.focus()
    const view = render(<Dialog onClose={() => {}} />)
    view.unmount()
    expect(document.activeElement).toBe(opener)
  })
})
