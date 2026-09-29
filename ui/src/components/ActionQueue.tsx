import { CircleUser, Clock, Inbox, Moon, ShieldQuestion } from 'lucide-react'

import type { ActionItem } from '../lib/actions'
import { elapsedSince } from '../lib/elapsed'

/**
 * What is waiting on the reader, across every workspace they belong to.
 *
 * See `docs/action-queue.md`. Deliberately not a feed: there is no read or
 * unread here, because an item leaves this list when somebody answers it rather
 * than when the reader has looked at it. Marking one as seen would be a promise
 * the queue cannot keep -- a colleague may answer it a second later, and the
 * mark would be the only trace of a decision that is no longer anybody's.
 *
 * Oldest first, and the age is stated rather than revealed on hover. A queue is
 * worked from the front, and how long something has been waiting is the fact
 * that decides what to do next -- which is exactly the fact a reader should not
 * have to go looking for.
 */
export default function ActionQueue({
  items,
  /** Names for the workspaces the rows come from. A row says which workspace it
   *  belongs to only when the reader spans more than one -- inside a single
   *  workspace the column is the same word repeated. */
  workspaceNames,
  onOpen,
  /** Shown when nothing is waiting. */
  quiet = 'Nothing is waiting on you.',
  /**
   * What time it is, for the ages and the expiries.
   *
   * Passed in rather than read here so this renders the same tree for the same
   * props -- a clock read during render makes the output depend on when it
   * happened, which is the one thing React is entitled to assume cannot happen.
   * The page that owns the polling already knows when it last heard from the
   * server, and that is the honest instant to render against.
   */
  now,
}: {
  items: ActionItem[]
  workspaceNames?: Record<string, string>
  onOpen?: (item: ActionItem) => void
  quiet?: string
  now: number
}) {
  if (items.length === 0) {
    return (
      <div className="flex flex-col items-center gap-2 py-10 text-center">
        <Inbox size={20} aria-hidden className="text-surface-400 dark:text-surface-500" />
        <p className="text-sm text-surface-600 dark:text-surface-400">{quiet}</p>
      </div>
    )
  }

  // Only worth a column when there is something to tell apart.
  const spansWorkspaces = new Set(items.map((i) => i.workspace_id)).size > 1

  return (
    <ul className="space-y-2">
      {items.map((item) => (
        <ActionRow
          key={item.id}
          item={item}
          workspace={
            spansWorkspaces
              ? (workspaceNames?.[item.workspace_id] ?? shortId(item.workspace_id))
              : null
          }
          onOpen={onOpen}
          now={now}
        />
      ))}
    </ul>
  )
}

function ActionRow({
  item,
  workspace,
  onOpen,
  now,
}: {
  item: ActionItem
  workspace: string | null
  onOpen?: (item: ActionItem) => void
  now: number
}) {
  const age = elapsedSince(item.id, now)
  const Icon = ICONS[iconKey(item.kind)]
  const summary = summarise(item)

  // A real `button` when there is somewhere to go, rather than a div wearing a
  // button's role: Enter and Space, the focus ring and the accessibility tree
  // all come from the platform, and reimplementing them is how one of the three
  // gets forgotten. A plain div when there is not, because a row that looks
  // clickable and is not is worse than one that never offered.
  const Row = onOpen ? 'button' : 'div'

  return (
    <li>
      <Row
        type={onOpen ? 'button' : undefined}
        onClick={onOpen ? () => onOpen(item) : undefined}
        className={`flex w-full items-start gap-3 rounded-md border border-surface-200 bg-surface-50 p-3 text-left dark:border-surface-800 dark:bg-surface-900/40 ${
          onOpen
            ? 'cursor-pointer transition-colors hover:border-surface-300 hover:bg-surface-100 dark:hover:border-surface-700 dark:hover:bg-surface-900'
            : ''
        }`}
      >
        <Icon
          size={16}
          aria-hidden
          className="mt-0.5 shrink-0 text-amber-600 dark:text-amber-400"
        />
        <div className="min-w-0 flex-1">
          <p className="truncate text-sm text-surface-900 dark:text-surface-100">
            <span className="font-medium">{label(item.kind)}</span>
            {summary && <span className="text-surface-700 dark:text-surface-300"> — {summary}</span>}
          </p>
          <p className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-xs text-surface-600 dark:text-surface-400">
            {age && (
              <span className="inline-flex items-center gap-1">
                <Clock size={11} aria-hidden />
                {/* Waiting rather than "ago": the reader is not being told when
                    it happened, they are being told how long it has been
                    nobody's answer. */}
                waiting {age.replace(/ ago$/, '')}
              </span>
            )}
            {workspace && <span className="truncate">in {workspace}</span>}
            {item.expires_at && <Expiry at={item.expires_at} now={now} />}
          </p>
        </div>
      </Row>
    </li>
  )
}

/**
 * When an item stops waiting on its own.
 *
 * Only shown once it is close enough to matter. An expiry a week out is noise
 * on every row; one inside the hour changes what a reader does next.
 */
function Expiry({ at, now }: { at: string; now: number }) {
  const left = new Date(at).getTime() - now
  if (!Number.isFinite(left) || left > 3_600_000) return null
  if (left <= 0) {
    return <span className="text-red-600 dark:text-red-400">expired</span>
  }
  const minutes = Math.max(1, Math.round(left / 60_000))
  return (
    <span className="text-amber-700 dark:text-amber-300">
      expires in {minutes} min{minutes === 1 ? '' : 's'}
    </span>
  )
}

/**
 * A kind as a person would read it.
 *
 * A fallback rather than a lookup that has to be complete: a kind nobody has
 * written a label for still renders as something, so a new producer is not a
 * blank row.
 */
function label(kind: string): string {
  switch (kind) {
    case 'approval.charge':
      return 'Charge to approve'
    case 'sleep':
      return 'Agent asleep'
    default:
      // `approval.something_else` reads better than the raw key, and says
      // enough: the producer builds the kind from the act it is asking about
      // (`requires` in docs/approvals.md), so the last segment is that word.
      return kind.split('.').at(-1)?.replace(/_/g, ' ').replace(/^./, (c) => c.toUpperCase()) ?? kind
  }
}

/**
 * Which glyph a kind gets.
 *
 * A lookup rather than a function returning a component: a component chosen
 * during render is a new type on every render as far as React is concerned, so
 * the icon unmounts and remounts each time.
 */
const ICONS = { approval: ShieldQuestion, sleep: Moon, other: CircleUser } as const

function iconKey(kind: string): keyof typeof ICONS {
  if (kind === 'sleep') return 'sleep'
  return kind.startsWith('approval.') ? 'approval' : 'other'
}

/**
 * The one line from the payload worth putting on the row.
 *
 * Read defensively: `payload` is free-form jsonb written by a producer this
 * component does not know, so anything missing or of the wrong type renders as
 * nothing rather than as `[object Object]`.
 */
function summarise(item: ActionItem): string | null {
  for (const key of ['question', 'summary', 'title', 'reason']) {
    const value = item.payload[key]
    if (typeof value === 'string' && value.trim() !== '') return value
  }
  return null
}

/** Enough of an id to tell two apart, for a workspace with no name to hand. */
function shortId(id: string): string {
  return id.slice(0, 8)
}
