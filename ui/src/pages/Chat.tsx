import { useEffect, useState } from 'react'
import { AssistantRuntimeProvider } from '@assistant-ui/react'
import { Plus } from 'lucide-react'

import Thread from '../components/Thread'
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
  const [agents, setAgents] = useState<Agent[]>([])
  const [sessions, setSessions] = useState<AgentSession[]>([])
  const [active, setActive] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  const { runtime, error: chatError } = useChatRuntime(active)

  // Agents and sessions are tenant-scoped, so switching tenant reloads both.
  const tenantId = state.status === 'authenticated' ? state.session.tenant_id : null
  useEffect(() => {
    let cancelled = false
    void (async () => {
      try {
        const [a, s] = await Promise.all([listAgents(), listSessions()])
        if (cancelled) return
        setAgents(a)
        setSessions(s)
        setActive(s[0]?.id ?? null)
        setError(null)
      } catch (e) {
        if (cancelled) return
        setError(e instanceof ApiError ? e.message : 'failed to load')
      }
    })()
    return () => {
      cancelled = true
    }
  }, [tenantId])

  async function start(agentId: string) {
    try {
      const session = await createSession(agentId)
      setSessions((prev) => [session, ...prev])
      setActive(session.id)
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to start session')
    }
  }

  const agentName = (id: string) => agents.find((a) => a.id === id)?.name ?? 'Agent'
  const shown = error ?? chatError

  return (
    <div className="flex h-full">
      <aside className="w-64 border-r border-gray-200 dark:border-gray-800 flex flex-col">
        <div className="p-3 border-b border-gray-200 dark:border-gray-800">
          <p className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-2">
            Start a session
          </p>
          {agents.length === 0 ? (
            <p className="text-xs text-gray-500 dark:text-gray-400">
              No agents yet — create one first.
            </p>
          ) : (
            <div className="space-y-1">
              {agents.map((agent) => (
                <button
                  key={agent.id}
                  onClick={() => void start(agent.id)}
                  className="w-full flex items-center gap-2 px-2 py-1.5 rounded-md text-sm text-left text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-800"
                >
                  <Plus size={14} className="shrink-0 text-gray-400" />
                  <span className="truncate">{agent.name}</span>
                </button>
              ))}
            </div>
          )}
        </div>
        <div className="flex-1 overflow-auto p-2 space-y-1">
          {sessions.map((session) => (
            <button
              key={session.id}
              onClick={() => setActive(session.id)}
              className={`w-full px-2 py-1.5 rounded-md text-sm text-left truncate ${
                session.id === active
                  ? 'bg-gray-100 dark:bg-gray-800 text-gray-900 dark:text-gray-100'
                  : 'text-gray-600 dark:text-gray-400 hover:bg-gray-50 dark:hover:bg-gray-800/50'
              }`}
            >
              {session.title || agentName(session.agent_id)}
            </button>
          ))}
        </div>
      </aside>

      <div className="flex-1 flex flex-col min-w-0">
        {shown && (
          <p
            className="px-6 py-2 text-sm text-red-600 dark:text-red-400 border-b border-gray-200 dark:border-gray-800"
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
    </div>
  )
}
