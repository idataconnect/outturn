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
  const [failed, setFailed] = useState<string | null>(null)

  useEffect(() => {
    let stopped = false
    // A compaction has landed (or the session changed), so the optimistic
    // "asked" state is spent: read the status again and let it decide.
    setAsked(false)
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

  // "asked" shows the progress the moment the button is pressed, rather than
  // waiting for `chat.compacting` to arrive on a later poll -- which, for a
  // fast compaction, can land in the same batch as `chat.compacted` and never
  // paint. It clears when the compaction lands (the effect above) or fails.
  if (compacting || asked) {
    return (
      <div className="border-b border-surface-200 bg-surface-100 px-6 py-2 text-sm text-surface-600 dark:border-surface-800 dark:bg-surface-800 dark:text-surface-400">
        Compacting this conversation…
      </div>
    )
  }

  if (!status || status.current) return null

  // Only the agent's own instructions changed when no skill did.
  const changes =
    status.skills.length > 0
      ? status.skills.map(describe).join(', ')
      : "This agent's instructions were changed"
  const it = status.skills.length === 1 ? 'it' : 'them'

  return (
    <div className="border-b border-surface-200 bg-surface-100 px-6 py-2 dark:border-surface-800 dark:bg-surface-800">
      <p className="text-sm text-surface-700 dark:text-surface-300" role="status">
        {changes}. To use {it},{' '}
        <button
          type="button"
          disabled={asked}
          onClick={() => {
            setAsked(true)
            setFailed(null)
            void compactSession(sessionId).catch((e) => {
              setAsked(false)
              setFailed(e instanceof ApiError ? e.message : 'the request did not go through')
            })
          }}
          className="underline underline-offset-2 disabled:opacity-50"
        >
          compact this session
        </button>{' '}
        or{' '}
        <Link to={`/sessions/new?agent=${agentId}`} className="underline underline-offset-2">
          start a new one
        </Link>
        .
      </p>
      {failed && (
        <p className="mt-1 text-xs text-red-600 dark:text-red-400" role="alert">
          Compacting did not start: {failed}
        </p>
      )}
    </div>
  )
}
