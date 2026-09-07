import { useEffect, useState } from 'react'
import { NavLink, useNavigate, useParams } from 'react-router'
import { AssistantRuntimeProvider } from '@assistant-ui/react'
import { Plus } from 'lucide-react'

import Thread from '../components/Thread'
import FilesPanel from '../components/FilesPanel'
import { ApiError } from '../lib/api'
import {
  createSession,
  listAgents,
  listSessions,
  type Agent,
  type AgentSession,
} from '../lib/chat'
import { useChatRuntime } from '../lib/useChatRuntime'
import { useSession } from '../lib/session'

export default function Chat() {
  const state = useSession()
  const navigate = useNavigate()
  // The URL owns the selection, so a session can be linked to, reloaded, and
  // reached with the back button.
  const { sessionId } = useParams<{ sessionId?: string }>()
  const [agents, setAgents] = useState<Agent[]>([])
  const [sessions, setSessions] = useState<AgentSession[]>([])
  const [error, setError] = useState<string | null>(null)

  const active = sessionId ?? null
  const { runtime, error: chatError } = useChatRuntime(active)

  // Agents and sessions are tenant-scoped, so switching tenant reloads both.
  // Selecting a session does not: that only changes which one is shown.
  const tenantId = state.status === 'authenticated' ? state.session.tenant_id : null
  // Offered as a way out only to somebody who could act on it. Sending a
  // reader to a page where they can look but not create leaves them exactly
  // where they started, having been told to do something they cannot.
  const canCreateAgents =
    state.status === 'authenticated' &&
    state.session.authorities.includes('agents:create')
  const [loaded, setLoaded] = useState(false)

  useEffect(() => {
    let cancelled = false
    setLoaded(false)
    void (async () => {
      try {
        const [a, s] = await Promise.all([listAgents(), listSessions()])
        if (cancelled) return
        setAgents(a)
        setSessions(s)
        setError(null)
      } catch (e) {
        if (cancelled) return
        setError(e instanceof ApiError ? e.message : 'failed to load')
      } finally {
        if (!cancelled) setLoaded(true)
      }
    })()
    return () => {
      cancelled = true
    }
  }, [tenantId])

  // Reconcile the URL against what this tenant can actually see, once loaded.
  useEffect(() => {
    if (!loaded) return

    // Land on the most recent session when none was named. Replace rather than
    // push, so the back button does not return to an empty /sessions that
    // immediately redirects here again.
    if (!sessionId) {
      if (sessions.length > 0) {
        void navigate(`/sessions/${sessions[0].id}`, { replace: true })
      }
      return
    }

    // A session in the URL this tenant cannot see -- a stale link, or one left
    // behind by a tenant switch -- would otherwise leave a blank pane with no
    // explanation.
    if (!sessions.some((session) => session.id === sessionId)) {
      setError('That session is not available in this tenant.')
      void navigate('/sessions', { replace: true })
    }
  }, [loaded, sessionId, sessions, navigate])

  async function start(agentId: string) {
    try {
      const session = await createSession(agentId)
      setSessions((prev) => [session, ...prev])
      void navigate(`/sessions/${session.id}`)
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to start session')
    }
  }

  const agentName = (id: string) => agents.find((a) => a.id === id)?.name ?? 'Agent'
  const shown = error ?? chatError

  return (
    <div className="flex h-full">
      <aside className="w-64 border-r border-surface-200 dark:border-surface-800 flex flex-col">
        <div className="p-3 border-b border-surface-200 dark:border-surface-800">
          <p className="text-xs font-medium text-surface-600 dark:text-surface-400 mb-2">
            Start a session
          </p>
          {agents.length === 0 ? (
            canCreateAgents ? (
              <NavLink
                to="/agents"
                className="flex items-center gap-2 px-2 py-1.5 rounded-md text-sm text-brand-700 dark:text-brand-400 hover:bg-surface-100 dark:hover:bg-surface-800"
              >
                <Plus size={14} className="shrink-0" />
                <span>Create an agent</span>
              </NavLink>
            ) : (
              <p className="text-xs text-surface-600 dark:text-surface-400">
                No agents yet. Ask an administrator to add one.
              </p>
            )
          ) : (
            <div className="space-y-1">
              {agents.map((agent) => (
                <button
                  key={agent.id}
                  onClick={() => void start(agent.id)}
                  className="w-full flex items-center gap-2 px-2 py-1.5 rounded-md text-sm text-left text-surface-700 dark:text-surface-300 hover:bg-surface-100 dark:hover:bg-surface-800"
                >
                  <Plus size={14} className="shrink-0 text-surface-400" />
                  <span className="truncate">{agent.name}</span>
                </button>
              ))}
            </div>
          )}
        </div>
        <div className="flex-1 overflow-auto p-2 space-y-1">
          {sessions.map((session) => (
            <NavLink
              key={session.id}
              to={`/sessions/${session.id}`}
              className={({ isActive }) =>
                `block w-full px-2 py-1.5 rounded-md text-sm text-left truncate ${
                  isActive
                    ? 'bg-brand-50 dark:bg-brand-950 text-brand-800 dark:text-brand-200'
                    : 'text-surface-600 dark:text-surface-400 hover:bg-surface-50 dark:hover:bg-surface-800/50'
                }`
              }
            >
              {session.title || agentName(session.agent_id)}
            </NavLink>
          ))}
        </div>
      </aside>

      <div className="flex-1 flex flex-col min-w-0">
        {shown && (
          <p
            className="px-6 py-2 text-sm text-red-600 dark:text-red-400 border-b border-surface-200 dark:border-surface-800"
            role="alert"
          >
            {shown}
          </p>
        )}
        <div className="flex-1 min-h-0">
          <AssistantRuntimeProvider runtime={runtime}>
            <Thread disabled={!active} />
          </AssistantRuntimeProvider>
        </div>
      </div>

      {active && <FilesPanel sessionId={active} />}
    </div>
  )
}
