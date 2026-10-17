import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { ChevronDown, Shuffle } from 'lucide-react'
import { useAuiState } from '@assistant-ui/react'

import {
  ModelChangesContext,
  listModelChanges,
  useModelChanges,
  type ModelChange,
} from '../lib/modelChanges'

/**
 * The replies in one conversation a fallback answered, read once and again
 * whenever a reply nobody has checked finishes -- which is when the gateway
 * will have recorded a change, if there was one.
 */
export function ModelChangesProvider({
  sessionId,
  children,
}: {
  sessionId: string | null
  children: ReactNode
}) {
  const [all, setAll] = useState<ModelChange[]>([])
  const checked = useRef(new Set<string>())
  const reading = useRef(false)
  const again = useRef(false)

  const reload = useCallback(async () => {
    if (!sessionId) return setAll([])
    if (reading.current) {
      again.current = true
      return
    }
    reading.current = true
    try {
      do {
        again.current = false
        setAll(await listModelChanges(sessionId))
      } while (again.current)
    } catch {
      // A note that does not show is only a note; the reply is still there.
    } finally {
      reading.current = false
    }
  }, [sessionId])

  useEffect(() => {
    checked.current = new Set()
    setAll([])
  }, [sessionId])

  const finished = useCallback(
    (replyId: string) => {
      if (checked.current.has(replyId)) return
      checked.current.add(replyId)
      void reload()
    },
    [reload],
  )

  const value = useMemo(
    () => ({ byReply: new Map(all.map((c) => [c.reply_id, c])), finished }),
    [all, finished],
  )
  return <ModelChangesContext.Provider value={value}>{children}</ModelChangesContext.Provider>
}

/**
 * Under a reply a fallback answered: that the model changed, and, opened,
 * from which to which and why. So staff know when the voice may differ.
 */
export function ReplyModelChange() {
  const { byReply, finished } = useModelChanges()
  const id = useAuiState((s) => s.message.id)
  const running = useAuiState((s) => s.message.status?.type === 'running')
  const [open, setOpen] = useState(false)

  useEffect(() => {
    if (id && !running) finished(id)
  }, [id, running, finished])

  const change = id ? byReply.get(id) : undefined
  if (!change) return null
  const from = change.configured_name ?? change.configured_model
  const to = change.served_name ?? change.served_model
  return (
    <div className="mt-1 text-xs text-surface-500 dark:text-surface-400">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
        className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 bg-surface-100 dark:bg-surface-800 hover:text-surface-700 dark:hover:text-surface-200"
      >
        <Shuffle size={12} aria-hidden />
        Agent model changed
        <ChevronDown size={12} aria-hidden className={open ? 'rotate-180' : undefined} />
      </button>
      {open && (
        <p className="mt-1 pl-1">
          Answered by {to} instead of {from}, because {from} {change.reason}.
        </p>
      )}
    </div>
  )
}
