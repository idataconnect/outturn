import { api } from './api'

/**
 * Something waiting for a person to do something about it.
 *
 * See `docs/action-queue.md`. Distinct from the event feed: an event stays true
 * for ever and is read or unread, while an item here is open until it is
 * settled and leaves the queue when a colleague answers it rather than when the
 * reader has looked at it.
 */
export type ActionItem = {
  id: string
  /** Which workspace it belongs to. Shown per row, because the queue spans
   *  every workspace the reader has a role in. */
  workspace_id: string
  /** What kind of thing is waiting. An approval is `approval.<act>`,
   *  built by the server from what it is asking about. */
  kind: string
  /** The event that produced it, where one did. */
  event_id: string | null
  /** Everything the row renders without a second fetch. Deliberately free
   *  form, and deliberately only what every target may see: the queue applies
   *  no agent narrowing, so what an agent said belongs behind `event_id`. */
  payload: Record<string, unknown>
  state: 'pending' | 'resolved' | 'cancelled' | 'expired'
  created_at: string
  expires_at: string | null
}

/**
 * What is waiting on whoever is asking, across every workspace they belong to.
 *
 * Global rather than per-workspace: the workspace somebody is *not* looking at
 * is exactly where an unseen decision sits. The server derives which workspaces
 * those are from the reader's own role grants, so there is nothing to pass.
 *
 * `wait` parks the request until something concerning them lands, and returns
 * the queue as it stands either way -- the queue is a set, so a timeout still
 * has a current answer worth returning.
 */
export function listActionItems(options: { wait?: boolean; signal?: AbortSignal } = {}) {
  const query = options.wait ? '?wait=true' : ''
  return api<{ items: ActionItem[] }>(`/v1/action-items${query}`, {
    signal: options.signal,
  })
}

/** How many are waiting, for the badge. `capped` means render "99+". */
export function countActionItems() {
  return api<{ count: number; capped: boolean }>('/v1/action-items/count')
}
