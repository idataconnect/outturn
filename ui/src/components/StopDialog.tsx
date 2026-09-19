import { useEffect, useId, useRef, useState, type FormEvent } from 'react'
import { OctagonX } from 'lucide-react'

import { ApiError } from '../lib/api'

/**
 * Asks why something is being stopped, then stops it.
 *
 * A hold with no reason explains nothing to whoever finds it, so the reason
 * is required and the button stays off until there is one. The dialog owns
 * the request: a failure is shown here, beside the reason that was typed,
 * rather than closing and leaving the reader to type it again.
 */
export default function StopDialog({
  subject,
  onStop,
  onClose,
}: {
  /** What is being stopped, as a phrase: "this whole workspace", an agent's name. */
  subject: string
  onStop: (reason: string) => Promise<void>
  onClose: () => void
}) {
  const [reason, setReason] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const dialog = useRef<HTMLDivElement>(null)
  const field = useRef<HTMLTextAreaElement>(null)
  const titleId = useId()

  // Escape closes, focus starts in the reason and stays inside, and goes back
  // where it came from on close -- as in FilePreview.
  useEffect(() => {
    const opener = document.activeElement as HTMLElement | null
    field.current?.focus()

    function onKey(event: KeyboardEvent) {
      if (event.key === 'Escape') {
        event.stopPropagation()
        onClose()
        return
      }
      if (event.key !== 'Tab') return
      const focusable = dialog.current?.querySelectorAll<HTMLElement>(
        'button:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
      )
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
      opener?.focus?.()
    }
  }, [onClose])

  async function onSubmit(event: FormEvent) {
    event.preventDefault()
    const given = reason.trim()
    if (!given || busy) return
    setBusy(true)
    setError(null)
    try {
      await onStop(given)
      onClose()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to stop')
      setBusy(false)
    }
  }

  return (
    <div
      className="fixed inset-0 z-40 flex items-center justify-center bg-black/50 p-4"
      onClick={(event) => {
        if (event.target === event.currentTarget && !busy) onClose()
      }}
      role="presentation"
    >
      <div
        ref={dialog}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        className="w-full max-w-md rounded-lg bg-white dark:bg-surface-900 border border-surface-200 dark:border-surface-700 shadow-xl"
      >
        <form onSubmit={(event) => void onSubmit(event)} className="p-5">
          <h2
            id={titleId}
            className="flex items-center gap-2 text-base font-semibold text-surface-900 dark:text-surface-100"
          >
            <OctagonX size={18} className="text-red-600 dark:text-red-400" aria-hidden />
            Stop {subject}?
          </h2>
          <p className="mt-2 text-sm text-surface-600 dark:text-surface-400">
            Nothing new starts until the hold is released. Say why, for whoever finds it.
          </p>

          <label className="mt-4 block text-sm font-medium text-surface-700 dark:text-surface-300">
            Reason
            <textarea
              ref={field}
              value={reason}
              onChange={(event) => setReason(event.target.value)}
              rows={3}
              disabled={busy}
              className="mt-1 block w-full rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 px-3 py-2 text-sm text-surface-900 dark:text-surface-100 focus:outline-none focus:ring-2 focus:ring-brand-500"
            />
          </label>

          {error && (
            <p className="mt-2 text-sm text-red-600 dark:text-red-400" role="alert">
              {error}
            </p>
          )}

          <div className="mt-5 flex justify-end gap-2">
            <button
              type="button"
              onClick={onClose}
              disabled={busy}
              className="px-4 py-2 rounded-md text-sm font-medium text-surface-700 dark:text-surface-300 hover:bg-surface-100 dark:hover:bg-surface-800"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={busy || !reason.trim()}
              className="px-4 py-2 rounded-md bg-red-600 text-white text-sm font-medium hover:bg-red-700 disabled:opacity-50"
            >
              {busy ? 'Stopping…' : 'Stop'}
            </button>
          </div>
        </form>
      </div>
    </div>
  )
}
