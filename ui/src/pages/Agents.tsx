import { useCallback, useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { MessageSquare, OctagonX, Play, Plus, Settings2, Trash2 } from 'lucide-react'

import { installTemplate, templateCatalog, type CatalogEntry } from '../lib/agentTemplates'

import { ApiError, api, allPages } from '../lib/api'
import { useSession } from '../lib/session'
import { recentSessionsOf, sessionName, type Agent, type AgentSession } from '../lib/chat'
import { useBreakpoint } from '../lib/useBreakpoint'
import AgentList from '../components/AgentList'
import Holds from '../components/Holds'
import StopDialog from '../components/StopDialog'
import {
  coveringAgent,
  listInhibitors,
  release,
  stopAgent,
  stopWorkspace,
  type Inhibitor,
} from '../lib/inhibitors'

/** How many of an agent's conversations its page lists. The rest are one
 *  click away in the sessions list. */
const RECENT = 5

/**
 * The workspace's agents: a list to find one in, and a page for the one found.
 *
 * People think of an agent before they think of a conversation with it, so
 * this is where one is started from as well as looked after. Its
 * configuration is a page of its own (`/agents/:id/edit`): what somebody who
 * came to talk to an agent needs is what it is for and a way to start, not its
 * system prompt.
 */
export default function Agents() {
  const state = useSession()
  const navigate = useNavigate()
  const { id } = useParams<{ id?: string }>()
  const breakpoint = useBreakpoint()
  const [agents, setAgents] = useState<Agent[]>([])
  const [sessions, setSessions] = useState<AgentSession[]>([])
  /** Why the recent list is missing, kept apart from `error`: the roster
   *  loads at the same time and clears that when it succeeds. */
  const [recentError, setRecentError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canCreate = authorities.includes('agents:create')
  const canUpdate = authorities.includes('agents:update')
  const canDelete = authorities.includes('agents:delete')
  const canStopAgent = authorities.includes('agents:inhibit')
  const canStopWorkspace = authorities.includes('workspaces:inhibit')
  const canReadSessions = authorities.includes('sessions:read')

  const [held, setHeld] = useState<Inhibitor[]>([])
  /** The operator's agents this workspace may have, and which it has. */
  const [catalog, setCatalog] = useState<CatalogEntry[]>([])

  async function refresh() {
    try {
      // Together, so the list never renders an agent as running while the
      // workspace holding it is still loading.
      const [list, holds, offered] = await Promise.all([
        allPages<Agent>('/v1/agents'),
        listInhibitors(),
        templateCatalog(),
      ])
      setAgents(list)
      setHeld(holds)
      // By name: the API pages in creation order, which is not one a person
      // looks things up in.
      setCatalog([...offered].sort((a, b) => a.name.localeCompare(b.name)))
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to load agents')
    } finally {
      setLoading(false)
    }
  }

  /** What the stop dialog is open for, if anything. */
  const [stopping, setStopping] = useState<'workspace' | Agent | null>(null)
  // Stable, so the dialog's focus handling is set up once rather than on
  // every render of this page.
  const closeStop = useCallback(() => setStopping(null), [])

  async function stop(what: 'workspace' | Agent, reason: string) {
    if (what === 'workspace') await stopWorkspace(reason)
    else await stopAgent(what.id, reason)
    await refresh()
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

  async function onAdd(entry: CatalogEntry) {
    try {
      const { agent_id } = await installTemplate(entry.template_id)
      await refresh()
      void navigate(`/agents/${agent_id}`)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to add agent')
    }
  }

  /** Whether an agent is one the operator requires every workspace to have. */
  function required(agent: Agent) {
    return catalog.some((e) => e.agent_id === agent.id && e.availability === 'required')
  }

  async function onDelete(agent: Agent) {
    if (!window.confirm(`Delete ${agent.name}? Its sessions go with it.`)) return
    try {
      await api<void>(`/v1/agents/${agent.id}`, { method: 'DELETE' })
      // Out of the list before the page moves, or a wide screen falls back
      // to the first agent in the old one -- which may be this one.
      setAgents((prev) => prev.filter((a) => a.id !== agent.id))
      void navigate('/agents', { replace: true })
      await refresh()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to delete agent')
    }
  }

  // On a wide screen an empty right half is a page that has not finished its
  // job, so with none named the first agent is shown -- without rewriting the
  // URL, which would pick an agent on the reader's behalf every time the list
  // changed. On a phone the list is the page.
  const selected = id ? agents.find((a) => a.id === id) : breakpoint !== 'phone' ? agents[0] : undefined
  const ownHold = selected
    ? held.find((i) => i.scope.level === 'agent' && i.scope.agent_id === selected.id)
    : undefined
  const selectedId = selected?.id
  // The selected agent's few most recent conversations, asked for by agent.
  // This read the workspace's whole session list and filtered it here, which
  // grew with every conversation ever had to show five of them.
  //
  // A reader who may see agents but not conversations still gets the page,
  // without the recent list -- not asked for, rather than asked for and
  // refused, so a failure that does happen is a real one and says so.
  useEffect(() => {
    if (!canReadSessions || !selectedId) {
      setSessions([])
      return
    }
    let current = true
    recentSessionsOf(selectedId, RECENT).then(
      (list) => {
        if (!current) return
        setSessions(list)
        setRecentError(null)
      },
      (e) => {
        if (current) setRecentError(e instanceof ApiError ? e.message : 'failed to load recent sessions')
      },
    )
    return () => {
      current = false
    }
  }, [workspaceId, canReadSessions, selectedId])
  const recent = sessions
  // A phone shows one half at a time: the list, or the agent picked from it.
  const showList = breakpoint !== 'phone' || !id
  const showDetail = breakpoint !== 'phone' || !!id

  return (
    <div className="flex h-full">
      {showList && (
        <aside className="flex flex-col w-full sm:w-64 shrink-0 sm:border-r border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900">
          {/* The same header the sessions list has: one full-width button and
              no title beside it, since the nav already says where this is.
              The heading stays for whoever navigates by headings. */}
          <h1 className="sr-only">Agents</h1>
          {canCreate && (
            <div className="p-3 border-b border-surface-200 dark:border-surface-800">
              <Link
                to="/agents/new"
                className="flex items-center justify-center gap-2 px-3 py-1.5 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium"
              >
                <Plus size={14} aria-hidden />
                New agent
              </Link>
            </div>
          )}
          <div className="flex-1 min-h-0 flex flex-col p-2">
            {loading ? (
              <p className="p-2 text-sm text-surface-600 dark:text-surface-400">Loading…</p>
            ) : (
              <AgentList
                agents={agents}
                href={(agent) => `/agents/${agent.id}`}
                current={selected?.id}
                empty={
                  <p className="p-2 text-sm text-surface-600 dark:text-surface-400">
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
                }
              />
            )}
          </div>
          {/* The operator's agents this workspace does not have: a default one
              it removed, or an optional one it never added. */}
          {canCreate && catalog.some((e) => !e.agent_id) && (
            <div className="p-3 border-t border-surface-200 dark:border-surface-800">
              <h2 className="text-xs font-medium uppercase tracking-wide text-surface-500 dark:text-surface-400">
                Available to add
              </h2>
              <ul className="mt-2 space-y-2">
                {catalog
                  .filter((e) => !e.agent_id)
                  .map((entry) => (
                    <li key={entry.template_id} className="flex items-start gap-2">
                      <div className="flex-1 min-w-0">
                        <p className="text-sm text-surface-800 dark:text-surface-200 truncate">
                          {entry.name}
                        </p>
                        {entry.description && (
                          <p className="text-xs text-surface-500 dark:text-surface-400 line-clamp-2">
                            {entry.description}
                          </p>
                        )}
                      </div>
                      <button
                        type="button"
                        onClick={() => void onAdd(entry)}
                        aria-label={`Add ${entry.name}`}
                        className="shrink-0 flex items-center gap-1 px-2 py-1 rounded-md border border-surface-300 dark:border-surface-700 text-xs text-surface-700 dark:text-surface-300 hover:bg-surface-50 dark:hover:bg-surface-800"
                      >
                        <Plus size={12} aria-hidden />
                        Add
                      </button>
                    </li>
                  ))}
              </ul>
            </div>
          )}
          {/* Workspace-wide, so it lives with the list rather than with any one
              agent: it stops every one of them. */}
          {canStopWorkspace && !held.some((i) => i.scope.level === 'workspace') && (
            <div className="p-3 border-t border-surface-200 dark:border-surface-800">
              <button
                type="button"
                onClick={() => setStopping('workspace')}
                className="w-full flex items-center justify-center gap-2 px-3 py-1.5 rounded-md border border-red-300 dark:border-red-900 text-red-700 dark:text-red-400 text-sm font-medium hover:bg-red-50 dark:hover:bg-red-950/40"
              >
                <OctagonX size={14} aria-hidden />
                Stop workspace
              </button>
            </div>
          )}
        </aside>
      )}

      {showDetail && (
        <div className="flex-1 min-w-0 overflow-auto p-6">
          {error && (
            <p className="mb-4 text-sm text-red-600 dark:text-red-400" role="alert">
              {error}
            </p>
          )}

          {/* Workspace-wide holds lead, because they explain why every agent
              is quiet -- reading them per agent would say the same thing N
              times. */}
          {held.some((i) => i.scope.level !== 'agent') && (
            <div className="mb-4">
              <Holds
                held={held.filter((i) => i.scope.level !== 'agent')}
                canRelease={mayRelease}
                onRelease={(hold) => void onRelease(hold)}
              />
            </div>
          )}

          {breakpoint === 'phone' && (
            <Link
              to="/agents"
              className="inline-block mb-3 text-sm text-surface-600 dark:text-surface-400"
            >
              ← Agents
            </Link>
          )}

          {selected ? (
            <div className="max-w-2xl">
              <div className="flex items-start justify-between gap-4">
                <div className="min-w-0">
                  <h2 className="text-2xl font-semibold text-surface-900 dark:text-surface-100 break-words">
                    {selected.name}
                    {!selected.enabled && (
                      <span className="ml-2 align-middle text-xs font-normal text-surface-600 dark:text-surface-400">
                        disabled
                      </span>
                    )}
                    {/* Stopped is not disabled: one is a hold somebody took and
                        can lift, the other is how the agent is configured. */}
                    {coveringAgent(held, selected.id).length > 0 && (
                      <span className="ml-2 align-middle text-xs font-normal text-red-700 dark:text-red-400">
                        stopped
                      </span>
                    )}
                  </h2>
                  <p className="text-xs font-mono text-surface-500 dark:text-surface-400">
                    {selected.slug}
                  </p>
                  {selected.template_id && (
                    <p className="mt-1 text-xs text-surface-500 dark:text-surface-400">
                      Provided by the operator
                      {required(selected) && ', and part of every workspace'}
                    </p>
                  )}
                </div>
                <div className="flex items-center gap-1 shrink-0">
                  <Link
                    to={`/agents/${selected.id}/edit`}
                    title={canUpdate ? 'Configure this agent' : 'See how this agent is configured'}
                    className="flex items-center gap-1.5 px-3 py-1.5 rounded-md text-sm text-surface-700 dark:text-surface-300 hover:bg-surface-100 dark:hover:bg-surface-800"
                  >
                    <Settings2 size={16} aria-hidden />
                    {canUpdate ? 'Configure' : 'Configuration'}
                  </Link>
                  {canStopAgent &&
                    (ownHold ? (
                      <button
                        onClick={() => void onRelease(ownHold)}
                        aria-label={`Start ${selected.name}`}
                        title="Release this agent's hold"
                        className="p-2 rounded-md text-surface-400 hover:text-green-700 dark:hover:text-green-400 hover:bg-surface-100 dark:hover:bg-surface-800"
                      >
                        <Play size={16} aria-hidden />
                      </button>
                    ) : (
                      <button
                        onClick={() => setStopping(selected)}
                        aria-label={`Stop ${selected.name}`}
                        title="Stop this agent"
                        className="p-2 rounded-md text-surface-400 hover:text-red-600 dark:hover:text-red-400 hover:bg-surface-100 dark:hover:bg-surface-800"
                      >
                        <OctagonX size={16} aria-hidden />
                      </button>
                    ))}
                  {canDelete && !required(selected) && (
                    <button
                      onClick={() => void onDelete(selected)}
                      aria-label={`Delete ${selected.name}`}
                      title="Delete this agent"
                      className="p-2 rounded-md text-surface-400 hover:text-red-600 dark:hover:text-red-400 hover:bg-surface-100 dark:hover:bg-surface-800"
                    >
                      <Trash2 size={16} aria-hidden />
                    </button>
                  )}
                </div>
              </div>

              <p className="mt-4 text-surface-700 dark:text-surface-300 whitespace-pre-line">
                {selected.description || (
                  <span className="text-surface-500 dark:text-surface-400">
                    No description yet.
                    {canUpdate && ' Say what this agent is for, so people know when to use it.'}
                  </span>
                )}
              </p>

              <div className="mt-6">
                {selected.can_chat ? (
                  <Link
                    to={`/sessions/new?agent=${selected.id}`}
                    className="inline-flex items-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium"
                  >
                    <MessageSquare size={16} aria-hidden />
                    New chat
                  </Link>
                ) : (
                  <p className="text-sm text-surface-600 dark:text-surface-400">
                    {selected.enabled
                      ? 'You cannot start a chat with this agent.'
                      : 'This agent is disabled, so no chat can be started with it.'}
                  </p>
                )}
              </div>

              {recentError && (
                <p className="mt-8 text-sm text-red-600 dark:text-red-400" role="alert">
                  Recent sessions could not be loaded: {recentError}
                </p>
              )}
              {recent.length > 0 && (
                <div className="mt-8">
                  <h3 className="text-xs font-medium uppercase tracking-wide text-surface-500 dark:text-surface-400">
                    Recent sessions
                  </h3>
                  <ul className="mt-2 space-y-1">
                    {recent.map((session) => (
                      <li key={session.id}>
                        <Link
                          to={`/sessions/${session.id}`}
                          className="block px-2 py-1.5 -mx-2 rounded-md text-sm text-surface-700 dark:text-surface-300 hover:bg-surface-50 dark:hover:bg-surface-800/50 truncate"
                        >
                          {sessionName(session)}
                        </Link>
                      </li>
                    ))}
                  </ul>
                </div>
              )}
            </div>
          ) : (
            !loading && (
              <p className="text-sm text-surface-600 dark:text-surface-400">
                {id ? 'That agent is not in this workspace.' : 'Choose an agent.'}
              </p>
            )
          )}
        </div>
      )}

      {stopping && (
        <StopDialog
          subject={stopping === 'workspace' ? 'this whole workspace' : stopping.name}
          onStop={(reason) => stop(stopping, reason)}
          onClose={closeStop}
        />
      )}
    </div>
  )
}
