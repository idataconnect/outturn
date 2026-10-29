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

/** A drawer: always mounted, trapping only while `open`. */
function Drawer({ open, onClose }: { open: boolean; onClose: () => void }) {
  const drawer = useRef<HTMLDivElement>(null)
  useFocusTrap(drawer, onClose, undefined, open)
  return (
    <>
      <button>page</button>
      <div ref={drawer}>
        <button>inside</button>
        <button>also inside</button>
      </div>
    </>
  )
}

describe('a trap that is only sometimes on', () => {
  it('does nothing while off, and takes focus when switched on', async () => {
    const onClose = vi.fn()
    const view = render(<Drawer open={false} onClose={onClose} />)
    const page = screen.getByRole('button', { name: 'page' })
    page.focus()
    await userEvent.keyboard('{Escape}')
    expect(onClose).not.toHaveBeenCalled()
    expect(document.activeElement).toBe(page)

    view.rerender(<Drawer open onClose={onClose} />)
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'inside' }))
    view.rerender(<Drawer open={false} onClose={onClose} />)
    expect(document.activeElement).toBe(page)
  })

  it('does not move focus when its caller renders with a new onClose', async () => {
    const view = render(<Drawer open onClose={() => {}} />)
    const also = screen.getByRole('button', { name: 'also inside' })
    also.focus()
    const onClose = vi.fn()
    view.rerender(<Drawer open onClose={onClose} />)
    expect(document.activeElement).toBe(also)
    await userEvent.keyboard('{Escape}')
    expect(onClose).toHaveBeenCalledOnce()
  })

  it('brings focus back in from the page behind on the next Tab', async () => {
    render(<Drawer open onClose={() => {}} />)
    screen.getByRole('button', { name: 'page' }).focus()
    await userEvent.tab()
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'inside' }))
  })
})
