import { api } from './api'

export type Agent = { id: string; name: string; slug: string }
export type AgentSession = { id: string; agent_id: string; title: string }

export type Message = {
  id: string
  seq: number
  role: 'user' | 'assistant' | 'system' | 'tool'
  content: string
  model: string | null
}

/** Events carry a copy of the message they announce. */
type ChatEvent =
  | { seq: number; kind: 'chat.message'; payload: Message }
  | { seq: number; kind: 'chat.error'; payload: { message: string } }

type PollResponse = {
  events: ChatEvent[]
  cursor: number
}

export const listAgents = () => api<Agent[]>('/v1/agents')
export const listSessions = () => api<AgentSession[]>('/v1/agent-sessions')

export const createSession = (agentId: string, title = '') =>
  api<AgentSession>('/v1/agent-sessions', {
    method: 'POST',
    body: JSON.stringify({ agent_id: agentId, title }),
  })

export const loadMessages = (sessionId: string) =>
  api<Message[]>(`/v1/agent-sessions/${sessionId}/messages`)

export const sendMessage = (sessionId: string, content: string) =>
  api<Message>(`/v1/agent-sessions/${sessionId}/messages`, {
    method: 'POST',
    body: JSON.stringify({ content }),
  })

/**
 * One long-poll round trip.
 *
 * Returns as soon as anything newer than `after` exists, otherwise parks on the
 * server until its timeout. The cursor is what makes a dropped notification
 * harmless: the next call asks for everything newer than what was seen, so a
 * miss costs one poll interval rather than an update.
 */
export const pollEvents = (sessionId: string, after: number, signal?: AbortSignal) =>
  api<PollResponse>(`/v1/events?session_id=${sessionId}&after=${after}`, { signal })
