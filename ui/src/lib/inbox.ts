import { createContext, useContext } from 'react'

import type { ActionItem } from './actions'

/**
 * What is waiting on the reader, shared by everything that shows it.
 *
 * One long poll per tab, held by `InboxProvider`, rather than one for the
 * navigation's count and another for the page: two would double the parked
 * requests for every open tab, and could disagree for a moment about a number
 * the reader is looking at twice.
 */
export type Inbox = {
  /** Oldest first, across every workspace the reader belongs to. */
  items: ActionItem[]
  /** For the badge: `capped` means more than it says. */
  count: number
  capped: boolean
  loaded: boolean
  /** Read again now, after this tab settled something. */
  refresh: () => void
}

export const InboxContext = createContext<Inbox>({
  items: [],
  count: 0,
  capped: false,
  loaded: false,
  refresh: () => {},
})

export function useInbox(): Inbox {
  return useContext(InboxContext)
}
