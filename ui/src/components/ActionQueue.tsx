import { useState } from 'react'
import { CircleUser, Clock, Inbox, Moon, ShieldQuestion } from 'lucide-react'

import { answerApproval, followUpOf, type ActionItem } from '../lib/actions'
import { waitingFor } from '../lib/elapsed'

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
  /** Called once any approval has been answered, so the owner of the list can
   *  fetch it again. */
  onAnswered,
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
  onAnswered?: () => void
  quiet?: string
  now: number
}) {
  // Declared before the early return: hooks run in the same order every render.
  const [answered, setAnswered] = useState<ReadonlySet<string>>(new Set())
  const [failures, setFailures] = useState<Record<string, string>>({})
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
  const nameOf = (item: ActionItem) =>
    spansWorkspaces ? (workspaceNames?.[item.workspace_id] ?? shortId(item.workspace_id)) : null

  // An item approved a moment ago stays in `items` until the owner refetches;
  // offering it again would be a second yes to something already settled.
  const open = items.filter((i) => !answered.has(i.id))
  const groups = approvalGroups(open, failures)

  return (
    <div className="space-y-2">
      {groups.map((group) => (
        <ApproveAll
          key={`${group[0].kind}/${group[0].workspace_id}`}
          items={group}
          workspace={nameOf(group[0])}
          failures={failures}
          onResult={(done, failed) => {
            setAnswered((prev) => new Set([...prev, ...done]))
            setFailures((prev) => {
              const next = { ...prev, ...failed }
              for (const id of done) delete next[id]
              return next
            })
            if (done.length > 0) onAnswered?.()
          }}
        />
      ))}
      <ul className="space-y-2">
        {open.map((item) => (
          <ActionRow
            key={item.id}
            item={item}
            workspace={nameOf(item)}
            onOpen={onOpen}
            now={now}
          />
        ))}
      </ul>
    </div>
  )
}

/**
 * Pending approvals grouped by kind within a workspace, keeping those with two
 * or more.
 *
 * Within a workspace because a standing yes belongs to one: a button that
 * answered for several workspaces at once would be a wider yes than any single
 * row suggests. A group that a failed answer has shrunk to one stays, so the
 * failure is still on screen next to what failed.
 *
 * Follow-ups are left out. A yes to one sends a held request again, and for one
 * whose outcome is not known that may be a second charge: each is decided on
 * its own, by somebody who has checked.
 */
function approvalGroups(items: ActionItem[], failures: Record<string, string>): ActionItem[][] {
  const groups = new Map<string, ActionItem[]>()
  for (const item of items) {
    if (!item.kind.startsWith('approval.') || item.state !== 'pending') continue
    if (followUpOf(item)) continue
    const key = `${item.kind}/${item.workspace_id}`
    groups.set(key, [...(groups.get(key) ?? []), item])
  }
  return [...groups.values()].filter((g) => g.length >= 2 || g.some((i) => failures[i.id]))
}

/**
 * One yes for several approvals of the same kind, with every one of them listed.
 *
 * See `docs/approvals.md`, *The checkbox goes*. The list is the point: a single
 * button over an unseen set is the widened yes the prompt stopped offering, so
 * each item the button answers is written out beside it, and nothing outside
 * the list is touched. Each is answered with the API's ordinary answer, one at
 * a time, so a failure is that item's and the rest still go through.
 */
function ApproveAll({
  items,
  workspace,
  failures,
  onResult,
}: {
  items: ActionItem[]
  workspace: string | null
  failures: Record<string, string>
  onResult: (done: string[], failed: Record<string, string>) => void
}) {
  const [busy, setBusy] = useState(false)

  async function approveAll() {
    setBusy(true)
    const done: string[] = []
    const failed: Record<string, string> = {}
    for (const item of items) {
      try {
        await answerApproval(item.id, { approved: true })
        done.push(item.id)
      } catch (e) {
        failed[item.id] = e instanceof Error ? e.message : 'that could not be sent'
      }
    }
    // Failed ones are left pending, and stay in the list so they can be sent again.
    setBusy(false)
    onResult(done, failed)
  }

  const kind = label(items[0].kind)
  return (
    <section
      aria-label={`Approve all: ${kind}`}
      className="rounded-md border border-amber-300 bg-amber-50 p-3 dark:border-amber-900 dark:bg-amber-950/40"
    >
      <p className="text-sm font-medium text-surface-900 dark:text-surface-100">
        {items.length} waiting: {kind}
        {workspace && <span className="font-normal"> in {workspace}</span>}
      </p>
      <ul className="mt-1.5 space-y-0.5 text-xs text-surface-700 dark:text-surface-300">
        {items.map((item) => (
          <li key={item.id}>
            {summarize(item) ?? shortId(item.id)}
            {failures[item.id] && (
              <span role="alert" className="text-red-700 dark:text-red-400">
                {' '}
                — {failures[item.id]}
              </span>
            )}
          </li>
        ))}
      </ul>
      <button
        type="button"
        disabled={busy}
        onClick={() => void approveAll()}
        className="mt-2 rounded-md bg-brand-600 px-2.5 py-1 text-xs font-medium text-white hover:bg-brand-700 disabled:opacity-50"
      >
        {busy ? 'Sending…' : items.length > 1 ? `Approve all ${items.length}` : 'Approve'}
      </button>
    </section>
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
  const age = waitingFor(item.id, now)
  const Icon = ICONS[iconKey(item.kind)]
  const summary = summarize(item)

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
            <span className="font-medium">{rowLabel(item)}</span>
            {summary && <span className="text-surface-700 dark:text-surface-300"> — {summary}</span>}
          </p>
          <p className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-xs text-surface-600 dark:text-surface-400">
            {age && (
              <span className="inline-flex items-center gap-1">
                <Clock size={11} aria-hidden />
                {/* Waiting rather than "ago": the reader is not being told when
                    it happened, they are being told how long it has been
                    nobody's answer. */}
                waiting {age}
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
    case 'policy.accept':
      return 'Operations waiting on a policy'
    default:
      // `approval.something_else` reads better than the raw key, and says
      // enough: the producer builds the kind from the act it is asking about
      // (`requires` in docs/approvals.md), so the last segment is that word.
      return kind.split('.').at(-1)?.replace(/_/g, ' ').replace(/^./, (c) => c.toUpperCase()) ?? kind
  }
}

/** A row's label: a follow-up says what became of the request, not that it
 *  needs approving. */
function rowLabel(item: ActionItem): string {
  // The bare act, `Charge`, by the same fallback any unlabeled kind gets.
  const what = label(`followup.${item.kind.split('.').at(-1)}`)
  switch (followUpOf(item)) {
    case 'unknown':
      return `${what}: sent, outcome not known`
    case 'unsent':
      return `${what}: approved, not sent`
    default:
      return label(item.kind)
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
function summarize(item: ActionItem): string | null {
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
