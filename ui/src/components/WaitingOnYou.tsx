import { useState } from 'react'
import { Link, useNavigate } from 'react-router'
import { ArrowRight } from 'lucide-react'

import ActionQueue from './ActionQueue'
import { useInbox } from '../lib/inbox'
import { paths } from '../lib/paths'
import { useSession } from '../lib/session'

/** How many of the oldest the card shows before sending the reader on. */
const SHOWN = 3

/**
 * The front of the inbox, where the day starts.
 *
 * Nothing at all when nothing is waiting: the navigation's count already says
 * so, and a card announcing an empty queue on a page about usage is a box to
 * read past every morning. When something is, the oldest few -- the front of
 * the queue, which is what gets worked first -- each opening in the inbox.
 */
export default function WaitingOnYou() {
  const { items, count, capped } = useInbox()
  const navigate = useNavigate()
  const state = useSession()
  // When the card was drawn, for the ages: read once, since a clock read while
  // rendering is a render that depends on when it happened.
  const [now] = useState(() => Date.now())

  if (items.length === 0) return null

  const names = Object.fromEntries(
    (state.status === 'authenticated' ? state.session.workspaces : []).map((w) => [
      w.workspace_id,
      w.name,
    ]),
  )
  const total = capped ? `${count}+` : String(count)

  return (
    <section
      aria-labelledby="waiting-on-you"
      className="rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 p-5"
    >
      <div className="flex items-baseline justify-between gap-4">
        <h2
          id="waiting-on-you"
          className="text-sm font-semibold text-surface-900 dark:text-surface-100"
        >
          Waiting on you <span className="font-normal text-surface-500">({total})</span>
        </h2>
        <Link
          to={paths.inbox}
          className="inline-flex items-center gap-1 text-sm text-brand-700 dark:text-brand-400 hover:underline underline-offset-2"
        >
          Open the inbox
          <ArrowRight size={14} aria-hidden />
        </Link>
      </div>
      <div className="mt-3">
        <ActionQueue
          items={items.slice(0, SHOWN)}
          workspaceNames={names}
          onOpen={(item) => void navigate(paths.inboxItem(item.id))}
          now={now}
        />
      </div>
    </section>
  )
}
