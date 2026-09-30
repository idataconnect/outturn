import { useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { AlarmClock, ArrowLeft, Clock, Inbox as InboxIcon, MessageSquare } from 'lucide-react'

import ApprovalPrompt from '../components/ApprovalPrompt'
import type { ActionItem } from '../lib/actions'
import { ApiError } from '../lib/api'
import { wakeSession } from '../lib/chat'
import { elapsedSince } from '../lib/elapsed'
import { useInbox } from '../lib/inbox'
import { describeItem } from '../lib/inboxKinds'
import { paths } from '../lib/paths'
import { useSession } from '../lib/session'
import { remaining } from '../lib/sleep'
import { useBreakpoint } from '../lib/useBreakpoint'

/**
 * What is waiting on the reader: approvals to give, agents asleep.
 *
 * A list beside the open item, like Agents and Sessions, because this is worked
 * through -- item after item, answering each -- rather than visited. Oldest
 * first, since a queue is worked from the front. Across every workspace the
 * reader belongs to: the workspace they are not looking at is exactly where an
 * unseen decision sits.
 *
 * Only what needs somebody to act. Something that is merely news -- a timer
 * that fired, a turn that failed -- does not belong here, or the inbox becomes
 * a feed nobody empties.
 */
export default function Inbox() {
  const { id } = useParams<{ id?: string }>()
  const { items, loaded } = useInbox()
  const breakpoint = useBreakpoint()
  const state = useSession()
  const workspaces = state.status === 'authenticated' ? state.session.workspaces : []
  const current = state.status === 'authenticated' ? state.session.workspace_id : null
  const spans = new Set(items.map((i) => i.workspace_id)).size > 1
  const workspaceName = (wid: string) => workspaces.find((w) => w.workspace_id === wid)?.name ?? wid.slice(0, 8)

  // The first waiting item opens on its own where there is room for both; on a
  // phone the list is the page until one is picked.
  const selected = id ? items.find((i) => i.id === id) : breakpoint !== 'phone' ? items[0] : undefined
  const showList = breakpoint !== 'phone' || !id
  const showDetail = breakpoint !== 'phone' || !!id

  return (
    <div className="flex h-full">
      {showList && (
        <aside className="flex flex-col w-full sm:w-72 shrink-0 sm:border-r border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900">
          <h1 className="px-4 pt-4 pb-2 text-sm font-medium text-surface-900 dark:text-surface-100">
            Inbox
          </h1>
          {!loaded ? (
            <p className="px-4 text-sm text-surface-600 dark:text-surface-400">Loading…</p>
          ) : items.length === 0 ? (
            <div className="flex flex-col items-center gap-2 px-4 py-10 text-center">
              <InboxIcon size={20} aria-hidden className="text-surface-400" />
              <p className="text-sm text-surface-600 dark:text-surface-400">Nothing is waiting on you.</p>
            </div>
          ) : (
            <ul className="flex-1 min-h-0 overflow-auto p-2 space-y-1">
              {items.map((item) => (
                <Row
                  key={item.id}
                  item={item}
                  active={item.id === selected?.id}
                  workspace={spans ? workspaceName(item.workspace_id) : null}
                />
              ))}
            </ul>
          )}
        </aside>
      )}
      {showDetail && (
        <div className="flex-1 min-w-0 overflow-auto">
          {selected ? (
            <Detail
              key={selected.id}
              item={selected}
              workspace={workspaceName(selected.workspace_id)}
              here={selected.workspace_id === current}
            />
          ) : id && loaded ? (
            // Opened by a link after somebody else answered it: say so rather
            // than showing an empty pane.
            <p className="p-6 text-sm text-surface-600 dark:text-surface-400">
              This has already been dealt with.{' '}
              <Link to={paths.inbox} className="underline underline-offset-2">
                Back to the inbox
              </Link>
            </p>
          ) : null}
        </div>
      )}
    </div>
  )
}

/**
 * How long something has been waiting, to follow the word "waiting": "for 5
 * minutes", or "just now". The phrase is written to stand alone ("5 minutes
 * ago", "Just now"), so it is trimmed and lowered to sit in a sentence.
 */
function waitingFor(id: string): string | null {
  const phrase = elapsedSince(id)
  if (!phrase) return null
  const trimmed = phrase.replace(/ ago$/, '')
  return trimmed === phrase ? trimmed.toLowerCase() : `for ${trimmed}`
}

function Row({ item, active, workspace }: { item: ActionItem; active: boolean; workspace: string | null }) {
  const { label, icon: Icon, summary } = describeItem(item)
  const age = waitingFor(item.id)
  return (
    <li>
      <Link
        to={paths.inboxItem(item.id)}
        aria-current={active ? 'page' : undefined}
        className={`flex items-start gap-2 rounded-md px-2 py-2 text-sm ${
          active
            ? 'bg-brand-50 dark:bg-brand-950 text-brand-900 dark:text-brand-100'
            : 'text-surface-800 dark:text-surface-200 hover:bg-surface-50 dark:hover:bg-surface-800/60'
        }`}
      >
        <Icon size={15} aria-hidden className="mt-0.5 shrink-0 text-amber-600 dark:text-amber-400" />
        <span className="min-w-0 flex-1">
          <span className="block truncate font-medium">{label}</span>
          {summary && (
            <span className="block truncate text-xs text-surface-600 dark:text-surface-400">{summary}</span>
          )}
          <span className="mt-0.5 flex gap-2 text-[11px] text-surface-500 dark:text-surface-400">
            {age && <span>waiting {age}</span>}
            {workspace && <span className="truncate">in {workspace}</span>}
          </span>
        </span>
      </Link>
    </li>
  )
}

function Detail({ item, workspace, here }: { item: ActionItem; workspace: string; here: boolean }) {
  const { kind, label, icon: Icon, summary, sessionId } = describeItem(item)
  const { refresh } = useInbox()
  const navigate = useNavigate()
  const age = waitingFor(item.id)

  // Settled here: re-read at once rather than waiting on the poll, and move
  // off an item that no longer exists.
  function settled() {
    refresh()
    void navigate(paths.inbox)
  }

  return (
    <div className="p-6 max-w-2xl space-y-5">
      <Link
        to={paths.inbox}
        className="sm:hidden flex items-center gap-1 text-sm text-surface-600 dark:text-surface-400"
      >
        <ArrowLeft size={14} aria-hidden />
        Inbox
      </Link>
      <div className="flex items-start gap-3">
        <Icon size={20} aria-hidden className="mt-1 shrink-0 text-amber-600 dark:text-amber-400" />
        <div className="min-w-0">
          <h2 className="text-xl font-semibold text-surface-900 dark:text-surface-100">{label}</h2>
          <p className="mt-1 flex flex-wrap gap-x-3 text-sm text-surface-600 dark:text-surface-400">
            {age && (
              <span className="inline-flex items-center gap-1">
                <Clock size={13} aria-hidden />
                Waiting {age}
              </span>
            )}
            <span>in {workspace}</span>
          </p>
        </div>
      </div>

      {/* An approval's card says its reason itself; said here as well it
          reads twice. */}
      {summary && kind !== 'approval' && (
        <p className="text-surface-800 dark:text-surface-200">{summary}</p>
      )}

      {kind === 'approval' && <ApprovalDetail item={item} onAnswered={settled} />}
      {kind === 'sleep' && sessionId && (
        <SleepDetail item={item} sessionId={sessionId} here={here} onWoken={settled} />
      )}

      {sessionId &&
        (here ? (
          <Link
            to={`/sessions/${sessionId}`}
            className="inline-flex items-center gap-1.5 text-sm text-brand-700 dark:text-brand-400 hover:underline underline-offset-2"
          >
            <MessageSquare size={14} aria-hidden />
            Open the conversation
          </Link>
        ) : (
          // Answering works from here whichever workspace it is in; reading the
          // conversation means being signed in to that workspace.
          <p className="text-sm text-surface-600 dark:text-surface-400">
            The conversation is in {workspace}. Switch to it to read the conversation.
          </p>
        ))}
    </div>
  )
}

/** What was asked for, in the words the request carried, and the answer. */
function ApprovalDetail({ item, onAnswered }: { item: ActionItem; onAnswered: () => void }) {
  const p = item.payload
  const request = [p.method, p.host, p.path].every((v) => typeof v === 'string')
    ? `${String(p.method)} ${String(p.host)}${String(p.path)}`
    : null
  const covers = p.covers as { field?: string; unit?: string } | undefined
  return (
    <div className="space-y-4">
      {request && (
        <p className="text-xs text-surface-600 dark:text-surface-400">
          The request: <code className="font-mono">{request}</code>
        </p>
      )}
      <ApprovalPrompt
        approval={{
          item_id: item.id,
          requires: typeof p.requires === 'string' ? p.requires : null,
          reason: typeof p.reason === 'string' ? p.reason : null,
          covers: covers ?? null,
        }}
        onAnswered={onAnswered}
      />
    </div>
  )
}

/** When the agent wakes, and the button that wakes it now. */
function SleepDetail({
  item,
  sessionId,
  here,
  onWoken,
}: {
  item: ActionItem
  sessionId: string
  /** Whether it is in the workspace the reader is signed in to. Waking goes
   *  through the conversation, which is only reachable from there. */
  here: boolean
  onWoken: () => void
}) {
  const [waking, setWaking] = useState(false)
  const [error, setError] = useState<string | null>(null)
  // When the pane opened. A countdown that ticks is the banner's job; here it
  // is a fact about the item, read once.
  const [now] = useState(() => Date.now())
  const until = typeof item.payload.until === 'string' ? item.payload.until : null

  async function wake() {
    setWaking(true)
    setError(null)
    try {
      await wakeSession(sessionId)
      onWoken()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'Could not wake it')
      setWaking(false)
    }
  }

  return (
    <div className="space-y-3">
      {until && (
        <p className="text-sm text-surface-700 dark:text-surface-300">
          Asleep {remaining(until, now)}. Anything sent to it meanwhile is kept, and
          answered together when it wakes.
        </p>
      )}
      {here && (
      <button
        type="button"
        onClick={() => void wake()}
        disabled={waking}
        className="inline-flex items-center gap-1.5 rounded-md bg-brand-700 px-3 py-1.5 text-sm font-medium text-white hover:bg-brand-600 disabled:opacity-60 dark:bg-brand-600 dark:hover:bg-brand-500"
      >
        <AlarmClock size={14} aria-hidden />
        {waking ? 'Waking…' : 'Wake now'}
      </button>
      )}
      {error && (
        <p className="text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}
    </div>
  )
}
