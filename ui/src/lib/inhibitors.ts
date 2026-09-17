import { api } from './api'

/**
 * A hold on work: a kill switch, and later a turn waiting for somebody.
 *
 * See `docs/inhibitors.md`. There may be several at once, which is why this is
 * a list rather than a flag -- two reasons to stop the same workspace are two
 * holds, and clearing one does not clear the other.
 */
export type Inhibitor = {
  id: string
  scope:
    | { level: 'platform' }
    | { level: 'workspace'; workspace_id: string }
    | { level: 'agent'; workspace_id: string; agent_id: string }
    | { level: 'session'; workspace_id: string; session_id: string }
  strength: 'suspended' | 'stopped'
  /** Why, in the holder's words. Required, so this is never empty. */
  reason: string
  held_by: string
  created_at: string
}

/** Everything held anywhere in the workspace being viewed. */
export function listInhibitors() {
  return api<Inhibitor[]>('/v1/inhibitors')
}

export function stopWorkspace(reason: string) {
  return api<Inhibitor>('/v1/workspace/stop', {
    method: 'POST',
    body: JSON.stringify({ reason }),
  })
}

export function stopAgent(agentId: string, reason: string) {
  return api<Inhibitor>(`/v1/agents/${agentId}/stop`, {
    method: 'POST',
    body: JSON.stringify({ reason }),
  })
}

/** Lifts one hold. Others on the same work keep applying. */
export function release(id: string) {
  return api<void>(`/v1/inhibitors/${id}`, { method: 'DELETE' })
}

/** The holds covering one agent: its own, and the workspace's. */
export function coveringAgent(held: Inhibitor[], agentId: string) {
  return held.filter(
    (i) =>
      i.scope.level === 'workspace' ||
      (i.scope.level === 'agent' && i.scope.agent_id === agentId),
  )
}
