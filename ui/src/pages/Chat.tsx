import { useEffect, useRef, useState } from 'react'
import { Plus, Send } from 'lucide-react'

import { ApiError, api } from '../lib/api'
import { useSession } from '../lib/session'

type Agent = { id: string; name: string; slug: string }
type AgentSession = { id: string; agent_id: string; title: string }
type Message = {
  id: string
  seq: number
  role: string
  content: string
  model: string | null
}

type PollResponse = {
  events: { seq: number; kind: string; payload: unknown }[]
  cursor: number
}

export default function Chat() {
  const state = useSession()
  const [agents, setAgents] = useState<Agent[]>([])
  const [sessions, setSessions] = useState<AgentSession[]>([])
  const [active, setActive] = useState<string | null>(null)
  const [messages, setMessages] = useState<Message[]>([])
  const [draft, setDraft] = useState('')
  const [sending, setSending] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const bottom = useRef<HTMLDivElement>(null)

  const tenantId = state.status === 'authenticated' ? state.session.tenant_id : null

  useEffect(() => {
    void (async () => {
      try {
        const [a, s] = await Promise.all([
          api<Agent[]>('/v1/agents'),
          api<AgentSession[]>('/v1/agent-sessions'),
        ])
        setAgents(a)
        setSessions(s)
        setActive((current) => current ?? s[0]?.id ?? null)
      } catch (e) {
        setError(e instanceof ApiError ? e.message : 'failed to load')
      }
    })()
  }, [tenantId])

  // Load history when the active session changes.
  useEffect(() => {
    if (!active) {
      setMessages([])
      return
    }
    void api<Message[]>(`/v1/agent-sessions/${active}/messages`)
      .then(setMessages)
      .catch(() => setMessages([]))
  }, [active])

  // Long poll for this session. A dropped notification degrades to the poll
  // timeout rather than a lost message, because the cursor asks for anything
  // newer than what has been seen.
  useEffect(() => {
    if (!active) return
    let cancelled = false
    let cursor = 0

    void (async () => {
      while (!cancelled) {
        try {
          const result = await api<PollResponse>(
            `/v1/events?session_id=${active}&after=${cursor}`,
          )
          if (cancelled) return
          cursor = result.cursor

          const arrived = result.events
            .filter((e) => e.kind === 'chat.message')
            .map((e) => e.payload as Message)

          if (arrived.length) {
            setMessages((prev) => {
              const seen = new Set(prev.map((m) => m.id))
              return [...prev, ...arrived.filter((m) => !seen.has(m.id))].sort(
                (a, b) => a.seq - b.seq,
              )
            })
          }

          const failed = result.events.find((e) => e.kind === 'chat.error')
          if (failed) {
            setError((failed.payload as { message: string }).message)
          }
        } catch {
          if (cancelled) return
          // Back off briefly so a downed API does not spin.
          await new Promise((r) => setTimeout(r, 2000))
        }
      }
    })()

    return () => {
      cancelled = true
    }
  }, [active])

  useEffect(() => {
    bottom.current?.scrollIntoView({ behavior: 'smooth' })
  }, [messages])

  async function startSession(agentId: string) {
    try {
      const session = await api<AgentSession>('/v1/agent-sessions', {
        method: 'POST',
        body: JSON.stringify({ agent_id: agentId, title: '' }),
      })
      setSessions((prev) => [session, ...prev])
      setActive(session.id)
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to start session')
    }
  }

  async function send(event: React.FormEvent) {
    event.preventDefault()
    if (!active || !draft.trim()) return
    setSending(true)
    const content = draft
    setDraft('')
    try {
      // The reply arrives over the event feed, not in this response.
      const message = await api<Message>(`/v1/agent-sessions/${active}/messages`, {
        method: 'POST',
        body: JSON.stringify({ content }),
      })
      setMessages((prev) =>
        prev.some((m) => m.id === message.id) ? prev : [...prev, message],
      )
      setError(null)
    } catch (e) {
      setDraft(content)
      setError(e instanceof ApiError ? e.message : 'failed to send')
    } finally {
      setSending(false)
    }
  }

  const agentName = (id: string) => agents.find((a) => a.id === id)?.name ?? 'Agent'

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
                  onClick={() => void startSession(agent.id)}
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
        <div className="flex-1 overflow-auto p-6 space-y-4">
          {!active ? (
            <p className="text-sm text-gray-500 dark:text-gray-400">
              Start a session to begin chatting.
            </p>
          ) : (
            messages.map((message) => (
              <div
                key={message.id}
                className={message.role === 'user' ? 'flex justify-end' : 'flex justify-start'}
              >
                <div
                  className={`max-w-[75%] px-4 py-2 rounded-lg text-sm whitespace-pre-wrap ${
                    message.role === 'user'
                      ? 'bg-gray-900 dark:bg-gray-100 text-white dark:text-gray-900'
                      : 'bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 text-gray-900 dark:text-gray-100'
                  }`}
                >
                  {message.content}
                </div>
              </div>
            ))
          )}
          {sending && (
            <p className="text-xs text-gray-500 dark:text-gray-400">Thinking…</p>
          )}
          <div ref={bottom} />
        </div>

        {error && (
          <p className="px-6 pb-2 text-sm text-red-600 dark:text-red-400" role="alert">
            {error}
          </p>
        )}

        <form
          onSubmit={send}
          className="flex gap-2 p-4 border-t border-gray-200 dark:border-gray-800"
        >
          <input
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            placeholder={active ? 'Message the agent…' : 'Start a session first'}
            disabled={!active}
            className="flex-1 px-3 py-2 rounded-md border border-gray-300 dark:border-gray-700 bg-white dark:bg-gray-950 text-gray-900 dark:text-gray-100 disabled:opacity-50"
          />
          <button
            type="submit"
            disabled={!active || !draft.trim()}
            className="flex items-center gap-2 px-4 py-2 rounded-md bg-gray-900 dark:bg-gray-100 text-white dark:text-gray-900 text-sm font-medium disabled:opacity-50"
          >
            <Send size={16} />
          </button>
        </form>
      </div>
    </div>
  )
}
