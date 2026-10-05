import { useEffect, useState } from 'react'
import { Link } from 'react-router'

import { compactSession, getPromptStatus, type PromptStatus, type StaleSkill } from '../lib/chat'
import { ApiError } from '../lib/api'

/** One skill's change, in words. */
function describe(s: StaleSkill): string {
  if (s.kept === null) return `${s.name} was added to this agent`
  if (s.live === null) return `${s.name} was removed from this agent`
  return `${s.name} v${s.live} is published`
}

/**
 * Says when a conversation is running on an older system prompt than a new one
 * would compose. A conversation keeps the prompt it was given until it
 * compacts, so an edit published meanwhile otherwise looks like one that did
 * not take. The two ways to take it up are offered here: compact this
 * conversation, or start a fresh one.
 */
export default function PromptBanner({
  sessionId,
  agentId,
  compacting,
  compactions,
}: {
  sessionId: string
  agentId: string
  /** A summary is being written right now, for either reason. */
  compacting: boolean
  /** Changes when a compaction lands, so the status is read again. */
  compactions: number
}) {
  const [status, setStatus] = useState<PromptStatus | null>(null)
  const [asked, setAsked] = useState(false)

  useEffect(() => {
    let stopped = false
    void getPromptStatus(sessionId)
      .then((s) => {
        if (!stopped) setStatus(s)
      })
      .catch(() => {
        if (!stopped) setStatus(null)
      })
    return () => {
      stopped = true
    }
  }, [sessionId, compactions])

  if (compacting) {
    return (
      <div className="border-b border-surface-200 bg-surface-100 px-6 py-2 text-sm text-surface-600 dark:border-surface-800 dark:bg-surface-800 dark:text-surface-400">
        Compacting this conversation…
      </div>
    )
  }

  if (!status || status.current || status.skills.length === 0) return null

  const changes = status.skills.map(describe).join(', ')

  return (
    <div className="border-b border-surface-200 bg-surface-100 px-6 py-2 dark:border-surface-800 dark:bg-surface-800">
      <p className="text-sm text-surface-700 dark:text-surface-300" role="status">
        {changes}. This conversation keeps what it started with until it compacts. To use the newer
        version,{' '}
        <button
          type="button"
          disabled={asked}
          onClick={() => {
            setAsked(true)
            void compactSession(sessionId).catch((e) => {
              setAsked(false)
              if (!(e instanceof ApiError)) throw e
            })
          }}
          className="underline underline-offset-2 disabled:opacity-50"
        >
          compact this one
        </button>{' '}
        or{' '}
        <Link to={`/sessions/new?agent=${agentId}`} className="underline underline-offset-2">
          start a new one
        </Link>
        .
      </p>
    </div>
  )
}
