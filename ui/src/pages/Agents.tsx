import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { Bot, OctagonX, Play, Plus, Trash2 } from 'lucide-react'

import { ApiError, api } from '../lib/api'
import { useSession } from '../lib/session'
import Holds from '../components/Holds'
import {
  coveringAgent,
  listInhibitors,
  release,
  stopAgent,
  stopWorkspace,
  type Inhibitor,
} from '../lib/inhibitors'
import type { Agent } from './AgentEditor'

/**
 * The workspace's agents, as a list.
 *
 * Creating and editing live on their own routes (`/agents/new`,
 * `/agents/:id`), so this page is only ever the list. An always-open editor
 * above the list made every visit look like a form to fill in, and left no
 * way to change an agent that already existed.
 */
export default function Agents() {
  const state = useSession()
  const [agents, setAgents] = useState<Agent[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canCreate = authorities.includes('agents:create')
  const canDelete = authorities.includes('agents:delete')
  const canStopAgent = authorities.includes('agents:inhibit')
  const canStopWorkspace = authorities.includes('workspaces:inhibit')
  // Only worth saying to somebody who can be looking at more than one workspace.
  // To everybody else there is no "currently viewing" -- there is only their
  // workspace -- and the sentence raises a question they cannot act on.
  const manyWorkspaces =
    state.status === 'authenticated' && state.session.workspaces.length > 1

  const [held, setHeld] = useState<Inhibitor[]>([])

  async function refresh() {
    try {
      // Together, so the list never renders an agent as running while the
      // workspace holding it is still loading.
      const [list, holds] = await Promise.all([api<Agent[]>('/v1/agents'), listInhibitors()])
      setAgents(list)
      setHeld(holds)
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to load agents')
    } finally {
      setLoading(false)
    }
  }

  /** Asks why, because a hold with no reason explains nothing to whoever finds it. */
  async function onStop(what: 'workspace' | Agent) {
    const subject = what === 'workspace' ? 'this whole workspace' : what.name
    const reason = window.prompt(`Why is ${subject} being stopped?`)
    if (reason === null) return
    if (!reason.trim()) {
      setError('a hold needs a reason')
      return
    }
    try {
      if (what === 'workspace') await stopWorkspace(reason)
      else await stopAgent(what.id, reason)
      await refresh()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to stop')
    }
  }

  async function onRelease(hold: Inhibitor) {
    try {
      await release(hold.id)
      await refresh()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to release')
    }
  }

  /** Whether this viewer may lift a given hold: agent holds are narrower. */
  function mayRelease(hold: Inhibitor) {
    return hold.scope.level === 'agent' ? canStopAgent : canStopWorkspace
  }

  // Agents are scoped to the active workspace, so switching reloads the list.
  const workspaceId = state.status === 'authenticated' ? state.session.workspace_id : null
  useEffect(() => {
    void refresh()
  }, [workspaceId])

  async function onDelete(agent: Agent) {
    if (!window.confirm(`Delete ${agent.name}? Its sessions go with it.`)) return
    try {
      await api<void>(`/v1/agents/${agent.id}`, { method: 'DELETE' })
      await refresh()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to delete agent')
    }
  }

  return (
    <div className="p-6 max-w-3xl">
      <div className="flex items-start justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold text-surface-900 dark:text-surface-100">Agents</h1>
          {manyWorkspaces && (
            <p className="mt-2 text-surface-600 dark:text-surface-400">
              Agents belong to the workspace you are currently viewing.
            </p>
          )}
        </div>
        {canStopWorkspace && !held.some((i) => i.scope.level === 'workspace') && (
          <button
            type="button"
            onClick={() => void onStop('workspace')}
            className="flex items-center gap-2 px-4 py-2 rounded-md border border-red-300 dark:border-red-900 text-red-700 dark:text-red-400 text-sm font-medium hover:bg-red-50 dark:hover:bg-red-950/40"
          >
            <OctagonX size={16} aria-hidden />
            Stop workspace
          </button>
        )}
        {canCreate && (
          <Link
            to="/agents/new"
            className="flex items-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium"
          >
            <Plus size={16} aria-hidden />
            New agent
          </Link>
        )}
      </div>

      {error && (
        <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      {/* Workspace-wide holds lead, because they explain why every agent below
          is quiet -- reading them per agent would say the same thing N times. */}
      {held.some((i) => i.scope.level !== 'agent') && (
        <div className="mt-4">
          <Holds
            held={held.filter((i) => i.scope.level !== 'agent')}
            canRelease={mayRelease}
            onRelease={(hold) => void onRelease(hold)}
          />
        </div>
      )}

      <div className="mt-6 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 overflow-hidden">
        {loading ? (
          <p className="p-4 text-sm text-surface-600 dark:text-surface-400">Loading…</p>
        ) : agents.length === 0 ? (
          <p className="p-4 text-sm text-surface-600 dark:text-surface-400">
            No agents yet.
            {canCreate && (
              <>
                {' '}
                <Link to="/agents/new" className="underline underline-offset-2">
                  Create one.
                </Link>
              </>
            )}
          </p>
        ) : (
          <ul className="divide-y divide-surface-200 dark:divide-surface-800">
            {agents.map((agent) => (
              <li key={agent.id} className="flex items-start gap-4 p-4">
                <Bot size={16} className="mt-1 shrink-0 text-surface-400" aria-hidden />
                <Link to={`/agents/${agent.id}`} className="flex-1 min-w-0 group">
                  <p className="text-sm font-medium text-surface-900 dark:text-surface-100 truncate group-hover:underline underline-offset-2">
                    {agent.name}
                    {!agent.enabled && (
                      <span className="ml-2 text-xs font-normal text-surface-600 dark:text-surface-400">
                        disabled
                      </span>
                    )}
                    {/* Stopped is not disabled: one is a hold somebody took and
                        can lift, the other is how the agent is configured. */}
                    {coveringAgent(held, agent.id).length > 0 && (
                      <span className="ml-2 text-xs font-normal text-red-700 dark:text-red-400">
                        stopped
                      </span>
                    )}
                  </p>
                  <p className="text-xs font-mono text-surface-600 dark:text-surface-400 truncate">
                    {agent.slug}
                  </p>
                  {agent.system_prompt && (
                    <p className="mt-1 text-xs text-surface-600 dark:text-surface-400 line-clamp-2">
                      {agent.system_prompt}
                    </p>
                  )}
                </Link>
                {canStopAgent &&
                  (held.some(
                    (i) => i.scope.level === 'agent' && i.scope.agent_id === agent.id,
                  ) ? (
                    <button
                      onClick={() => {
                        const own = held.find(
                          (i) => i.scope.level === 'agent' && i.scope.agent_id === agent.id,
                        )
                        if (own) void onRelease(own)
                      }}
                      aria-label={`Start ${agent.name}`}
                      title="Release this agent's hold"
                      className="p-2 rounded-md text-surface-400 hover:text-green-700 dark:hover:text-green-400 hover:bg-surface-100 dark:hover:bg-surface-800"
                    >
                      <Play size={16} aria-hidden />
                    </button>
                  ) : (
                    <button
                      onClick={() => void onStop(agent)}
                      aria-label={`Stop ${agent.name}`}
                      title="Stop this agent"
                      className="p-2 rounded-md text-surface-400 hover:text-red-600 dark:hover:text-red-400 hover:bg-surface-100 dark:hover:bg-surface-800"
                    >
                      <OctagonX size={16} aria-hidden />
                    </button>
                  ))}
                {canDelete && (
                  <button
                    onClick={() => void onDelete(agent)}
                    aria-label={`Delete ${agent.name}`}
                    className="p-2 rounded-md text-surface-400 hover:text-red-600 dark:hover:text-red-400 hover:bg-surface-100 dark:hover:bg-surface-800"
                  >
                    <Trash2 size={16} aria-hidden />
                  </button>
                )}
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  )
}
