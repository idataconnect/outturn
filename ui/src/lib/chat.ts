import { api } from './api'

export type Agent = { id: string; name: string; slug: string }
export type AgentSession = { id: string; agent_id: string; title: string }

/** A tool the agent ran, labelled by the agent with what it was doing. */
export type ToolCallRecord = {
  id: string
  name: string
  /** Present continuous, written for the user: "Checking today's date". */
  action: string
  /** What came back, in full. Never sent to the model. Absent until the
   *  tool finishes, and empty for tools with nothing worth showing. */
  details?: string
  is_error?: boolean
}

export type Message = {
  /** UUIDv7: ordering is carried by the id, so there is no separate sequence. */
  id: string
  role: 'user' | 'assistant' | 'system' | 'tool'
  content: string
  /** What the agent did on the way to this reply. */
  metadata: { tool_calls?: ToolCallRecord[] }
  /** How many deltas `content` already accounts for. */
  delta_next: number
  model: string | null
}

/**
 * A transcript and the event cursor it was read at.
 *
 * Both come from one snapshot on the server, so polling from `cursor` picks up
 * exactly where the content stops -- no event is replayed into a message that
 * already contains it, and none is skipped.
 */
export type History = {
  messages: Message[]
  cursor: string
}

export type ChatEvent =
  /** A message exists. Assistant replies arrive empty and are streamed into. */
  | { id: string; kind: 'chat.message'; payload: Message }
  /** A fragment of a message's content, in order. */
  | {
      id: string
      kind: 'chat.delta'
      payload: { message_id: string; idx: number; text: string }
    }
  /**
   * A message is complete. Carries no content: the client has already rendered
   * the deltas, and re-sending the text would invite a replace that flashes if
   * the two ever differed.
   */
  /** The agent started a tool. Arrives before the reply that used it. */
  | {
      id: string
      kind: 'chat.tool'
      payload: { message_id: string; call: ToolCallRecord }
    }
  /** That tool finished, with what it produced. */
  | {
      id: string
      kind: 'chat.tool_result'
      payload: {
        message_id: string
        id: string
        details: string
        is_error: boolean
      }
    }
  | { id: string; kind: 'chat.done'; payload: { message_id: string } }
  | { id: string; kind: 'chat.error'; payload: { message: string } }

type PollResponse = {
  events: ChatEvent[]
  cursor: string
}

export const listAgents = () => api<Agent[]>('/v1/agents')
export const listSessions = () => api<AgentSession[]>('/v1/agent-sessions')

export const createSession = (agentId: string, title = '') =>
  api<AgentSession>('/v1/agent-sessions', {
    method: 'POST',
    body: JSON.stringify({ agent_id: agentId, title }),
  })

export const loadHistory = (sessionId: string) =>
  api<History>(`/v1/agent-sessions/${sessionId}/messages`)

/**
 * The sender's IANA timezone, as the browser understands it.
 *
 * Sent with each message rather than stored on the account: it is where the
 * user is now, and the agent's clock should follow them rather than follow
 * where they signed up.
 */
const timezone = (): string | undefined => {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || undefined
  } catch {
    // A browser without a resolvable zone leaves the agent on UTC, which is
    // wrong but honest -- better than sending a guess.
    return undefined
  }
}

export const sendMessage = (sessionId: string, content: string) =>
  api<Message>(`/v1/agent-sessions/${sessionId}/messages`, {
    method: 'POST',
    body: JSON.stringify({ content, timezone: timezone() }),
  })

/**
 * One long-poll round trip.
 *
 * Returns as soon as anything newer than `after` exists, otherwise parks on the
 * server until its timeout. The cursor is what makes a dropped notification
 * harmless: the next call asks for everything newer than what was seen, so a
 * miss costs one poll interval rather than an update.
 */
export const pollEvents = (sessionId: string, after: string, signal?: AbortSignal) =>
  api<PollResponse>(`/v1/events?session_id=${sessionId}&after=${after}`, { signal })
