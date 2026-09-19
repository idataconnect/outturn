import { useEffect, type RefObject } from 'react'

/**
 * Everything inside a dialog that a Tab can land on.
 *
 * One list, shared, because the failure it prevents is silent: a dialog whose
 * selector omits the one control it happens to contain lets Tab walk onto the
 * page behind it, and nothing about the dialog looks wrong until somebody
 * tries. Two hand-kept copies had already drifted -- one catching links but
 * not textareas, the other the reverse -- so neither trapped both.
 */
const FOCUSABLE = [
  'a[href]',
  'button:not([disabled])',
  'input:not([disabled])',
  'textarea:not([disabled])',
  'select:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(', ')

/**
 * Escape closes, Tab stays inside, and focus goes back where it came from.
 *
 * A modal that can be tabbed out of drops the caret on a page that is not
 * reachable by pointer, which is worse than nothing; one that cannot be
 * dismissed from the keyboard is one somebody is trapped in. Both are the
 * same concern -- where focus is allowed to be while this is open -- so they
 * live together rather than being re-derived per dialog.
 *
 * @param dialog The element focus is kept within.
 * @param onClose Called on Escape. Closing is the caller's to do.
 * @param initial Focused on open. Defaults to the first thing in the dialog,
 *   which is right for a viewer; a form passes its field instead, so typing
 *   can start without a Tab.
 */
export function useFocusTrap(
  dialog: RefObject<HTMLElement | null>,
  onClose: () => void,
  initial?: RefObject<HTMLElement | null>,
) {
  useEffect(() => {
    const opener = document.activeElement as HTMLElement | null
    const target =
      initial?.current ?? dialog.current?.querySelector<HTMLElement>(FOCUSABLE)
    target?.focus()

    function onKey(event: KeyboardEvent) {
      if (event.key === 'Escape') {
        event.stopPropagation()
        onClose()
        return
      }
      if (event.key !== 'Tab') return
      const focusable = dialog.current?.querySelectorAll<HTMLElement>(FOCUSABLE)
      if (!focusable || focusable.length === 0) return
      const first = focusable[0]
      const last = focusable[focusable.length - 1]
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault()
        last.focus()
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault()
        first.focus()
      }
    }

    document.addEventListener('keydown', onKey, true)
    return () => {
      document.removeEventListener('keydown', onKey, true)
      // Put back where it came from, so closing does not dump the caret at
      // the top of the document.
      opener?.focus?.()
    }
    // The refs are stable; re-running on a new onClose is what keeps a stale
    // closure from closing the wrong thing.
  }, [dialog, onClose, initial])
}
