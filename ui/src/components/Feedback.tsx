import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react'
import { ThumbsDown, ThumbsUp } from 'lucide-react'
import { useAuiState } from '@assistant-ui/react'

import { ApiError } from '../lib/api'
import { actorName } from '../lib/actors'
import {
  FeedbackContext,
  giveFeedback,
  listFeedback,
  useFeedback,
  withdrawFeedback,
  type Feedback,
  type Opened,
  type Verdict,
} from '../lib/feedback'

/**
 * Everyone's verdicts on one conversation, read once for the header and every
 * reply in it.
 */
export function FeedbackProvider({
  sessionId,
  children,
}: {
  sessionId: string | null
  children: ReactNode
}) {
  const [all, setAll] = useState<Feedback[]>([])
  const [opened, open] = useState<Opened>(null)

  const reload = useCallback(async () => {
    if (!sessionId) return setAll([])
    try {
      setAll(await listFeedback(sessionId))
    } catch {
      // Not this page's failure to report: the conversation is still there,
      // and a verdict that does not show is one somebody can give again.
      setAll([])
    }
  }, [sessionId])

  useEffect(() => {
    open(null)
    void reload()
  }, [reload])

  const value = useMemo(
    () => ({ sessionId, all, opened, open, reload }),
    [sessionId, all, opened, reload],
  )
  return <FeedbackContext.Provider value={value}>{children}</FeedbackContext.Provider>
}

const thumb =
  'p-1 rounded text-surface-400 hover:text-surface-700 dark:hover:text-surface-200 hover:bg-surface-100 dark:hover:bg-surface-800'

/**
 * The thumbs beside a reply's copy button. Shown with it on hover, and kept
 * once the reader has given a verdict here, so what they said stays visible.
 */
export function ReplyThumbs({ shown }: { shown: boolean }) {
  const { sessionId, all, opened, open } = useFeedback()
  const messageId = useAuiState((s) => s.message.id)
  const running = useAuiState((s) => s.message.status?.type === 'running')
  if (!sessionId || running) return null

  const mine = all.find((f) => f.mine && f.message_id === messageId)
  const visible = shown || !!mine || opened?.messageId === messageId
  const button = (verdict: Verdict) => {
    const Icon = verdict === 'up' ? ThumbsUp : ThumbsDown
    const chosen = mine?.verdict === verdict
    return (
      <button
        type="button"
        aria-pressed={chosen}
        aria-label={verdict === 'up' ? 'This went well' : 'This went wrong'}
        title={verdict === 'up' ? 'This went well' : 'This went wrong'}
        onClick={() => open({ messageId, verdict })}
        className={`${thumb} ${chosen ? 'text-brand-600 dark:text-brand-400' : ''}`}
      >
        <Icon size={13} aria-hidden fill={chosen ? 'currentColor' : 'none'} />
      </button>
    )
  }
  return (
    <span
      className={`flex items-center transition-opacity ${visible ? 'opacity-100' : 'opacity-0'} focus-within:opacity-100`}
    >
      {button('up')}
      {button('down')}
    </span>
  )
}

/**
 * Under a reply: the panel a thumb opens, and what everybody said about it.
 *
 * The verdict can be about this reply or the whole conversation, chosen here
 * rather than by a second set of buttons somewhere else: a person rating a
 * conversation is usually looking at the reply that decided it.
 */
export function ReplyFeedback() {
  const { sessionId, all, opened, open, reload } = useFeedback()
  const messageId = useAuiState((s) => s.message.id)
  const here = all.filter((f) => f.message_id === messageId)
  const isOpen = opened?.messageId === messageId

  return (
    <>
      {isOpen && sessionId && (
        <Panel
          sessionId={sessionId}
          messageId={messageId}
          verdict={opened.verdict}
          existing={all.filter((f) => f.mine)}
          onClose={() => open(null)}
          onSaved={reload}
        />
      )}
      {here.length > 0 && !isOpen && (
        <ul className="mt-1 space-y-0.5">
          {here.map((f) => (
            <Said key={f.id} feedback={f} />
          ))}
        </ul>
      )}
    </>
  )
}

function Said({ feedback }: { feedback: Feedback }) {
  const Icon = feedback.verdict === 'up' ? ThumbsUp : ThumbsDown
  const who = feedback.mine ? 'You' : (actorName(feedback.by) ?? 'Somebody')
  return (
    <li className="flex items-start gap-1.5 text-xs text-surface-500 dark:text-surface-400">
      <Icon size={12} aria-hidden className="mt-0.5 shrink-0" />
      <span>
        <span className="font-medium">{who}</span>
        {feedback.note ? `: ${feedback.note}` : ''}
      </span>
    </li>
  )
}

function Panel({
  sessionId,
  messageId,
  verdict,
  existing,
  onClose,
  onSaved,
}: {
  sessionId: string
  messageId: string
  verdict: Verdict
  existing: Feedback[]
  onClose: () => void
  onSaved: () => Promise<void>
}) {
  const onReply = existing.find((f) => f.message_id === messageId)
  const onWhole = existing.find((f) => f.message_id === null)
  const [whole, setWhole] = useState(false)
  const current = whole ? onWhole : onReply
  const [note, setNote] = useState(onReply?.note ?? '')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  // The note follows the target: switching to the whole conversation shows
  // what the reader said about it before, rather than carrying this reply's
  // words across to it.
  useEffect(() => {
    setNote((whole ? onWhole : onReply)?.note ?? '')
  }, [whole, onWhole, onReply])

  async function run(action: () => Promise<void>) {
    setBusy(true)
    try {
      await action()
      await onSaved()
      onClose()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'could not save')
    } finally {
      setBusy(false)
    }
  }

  const target = whole ? null : messageId
  return (
    <div className="mt-2 w-full max-w-md rounded-lg border border-surface-200 dark:border-surface-700 bg-white dark:bg-surface-900 p-3 space-y-3 text-sm">
      <fieldset>
        <legend className="mb-1 text-xs text-surface-500 dark:text-surface-400">About</legend>
        <div className="flex gap-4">
          {[
            { value: false, label: 'This reply' },
            { value: true, label: 'The whole conversation' },
          ].map((o) => (
            <label key={o.label} className="flex items-center gap-1.5 cursor-pointer">
              <input
                type="radio"
                name={`feedback-about-${messageId}`}
                checked={whole === o.value}
                onChange={() => setWhole(o.value)}
              />
              {o.label}
            </label>
          ))}
        </div>
      </fieldset>
      <textarea
        value={note}
        onChange={(e) => setNote(e.target.value)}
        rows={3}
        aria-label={verdict === 'down' ? 'What went wrong' : 'What went well'}
        placeholder={
          verdict === 'down' ? 'What went wrong? Optional.' : 'What went well? Optional.'
        }
        className="w-full px-2 py-1.5 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 text-surface-900 dark:text-surface-100 focus:outline-none focus:ring-2 focus:ring-brand-500/40"
      />
      {error && (
        <p className="text-xs text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}
      <div className="flex items-center gap-2">
        <button
          type="button"
          disabled={busy}
          onClick={() => void run(() => giveFeedback(sessionId, { message_id: target, verdict, note }))}
          className="inline-flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-md bg-brand-600 text-white hover:bg-brand-700 disabled:opacity-50"
        >
          {verdict === 'up' ? <ThumbsUp size={13} aria-hidden /> : <ThumbsDown size={13} aria-hidden />}
          {current ? 'Update' : 'Save'}
        </button>
        <button
          type="button"
          onClick={onClose}
          className="px-3 py-1.5 text-sm rounded-md border border-surface-300 dark:border-surface-600 hover:bg-surface-100 dark:hover:bg-surface-800"
        >
          Cancel
        </button>
        {current && (
          <button
            type="button"
            disabled={busy}
            onClick={() => void run(() => withdrawFeedback(sessionId, target))}
            className="ml-auto text-xs text-surface-500 hover:text-red-600 hover:underline"
          >
            Remove my verdict
          </button>
        )}
      </div>
    </div>
  )
}

/**
 * In the header: how the conversation as a whole was rated, if anybody did.
 * The notes are on hover, since the header has no room for them.
 */
export function ConversationVerdicts() {
  const { all } = useFeedback()
  const whole = all.filter((f) => f.message_id === null)
  if (whole.length === 0) return null
  const up = whole.filter((f) => f.verdict === 'up').length
  const down = whole.length - up
  const detail = whole
    .map((f) => {
      const who = f.mine ? 'You' : (actorName(f.by) ?? 'Somebody')
      return `${who}: ${f.verdict === 'up' ? 'went well' : 'went wrong'}${f.note ? ` -- ${f.note}` : ''}`
    })
    .join('\n')
  return (
    <span
      className="hidden sm:flex shrink-0 items-center gap-2 text-xs text-surface-400 dark:text-surface-500"
      title={detail}
      aria-label={`The conversation was rated: ${detail}`}
    >
      {up > 0 && (
        <span className="flex items-center gap-0.5">
          <ThumbsUp size={12} aria-hidden /> {up}
        </span>
      )}
      {down > 0 && (
        <span className="flex items-center gap-0.5">
          <ThumbsDown size={12} aria-hidden /> {down}
        </span>
      )}
    </span>
  )
}
