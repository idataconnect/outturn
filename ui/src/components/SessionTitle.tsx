import { useEffect, useRef, useState } from 'react'
import { Pencil } from 'lucide-react'

import { UNNAMED_SESSION } from '../lib/chat'
import { iconButton } from '../lib/buttons'

/**
 * A session's name, and the way to change it.
 *
 * Shown as text with a pencil beside it; the pencil, or a click on the name,
 * turns it into a field. Enter keeps, Escape drops, and leaving the field
 * keeps too, because that is what happens to a name typed and then
 * forgotten about. An empty name is allowed: it means "unnamed", and the
 * namer will offer one after the next turn.
 */
export default function SessionTitle({
  title,
  canRename,
  onRename,
}: {
  title: string
  canRename: boolean
  onRename: (title: string) => void
}) {
  const [editing, setEditing] = useState(false)
  const [draft, setDraft] = useState(title)
  const input = useRef<HTMLInputElement>(null)

  useEffect(() => {
    if (editing) input.current?.select()
  }, [editing])

  function begin() {
    if (!canRename) return
    setDraft(title === UNNAMED_SESSION ? '' : title)
    setEditing(true)
  }

  function keep() {
    setEditing(false)
    const next = draft.trim()
    if (next !== (title === UNNAMED_SESSION ? '' : title)) onRename(next)
  }

  if (editing) {
    return (
      <input
        ref={input}
        value={draft}
        maxLength={80}
        placeholder={UNNAMED_SESSION}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={keep}
        onKeyDown={(e) => {
          if (e.key === 'Enter') keep()
          if (e.key === 'Escape') setEditing(false)
        }}
        aria-label="Session name"
        className="flex-1 min-w-0 px-2 py-0.5 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 text-sm text-surface-900 dark:text-surface-100 focus:outline-none focus:ring-2 focus:ring-brand-500/40"
      />
    )
  }

  return (
    <div className="flex-1 min-w-0 flex items-center gap-1.5">
      <button
        type="button"
        onClick={begin}
        disabled={!canRename}
        title={canRename ? 'Rename session' : undefined}
        className="min-w-0 truncate text-sm font-medium text-surface-800 dark:text-surface-200 text-left disabled:cursor-default"
      >
        {title}
      </button>
      {canRename && (
        <button
          type="button"
          onClick={begin}
          aria-label="Rename session"
          className={`shrink-0 ${iconButton}`}
        >
          <Pencil size={13} aria-hidden />
        </button>
      )}
    </div>
  )
}
