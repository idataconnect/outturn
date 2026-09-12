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

/** One piece of a reply, in the order it was produced. A call names the tool
 *  it refers to by id; the tool itself is in `metadata.tool_calls`, so nothing
 *  about a call is written down twice. */
export type MessagePart =
  | { type: 'text'; text: string }
  | { type: 'call'; id: string }

export type Message = {
  /** UUIDv7: ordering is carried by the id, so there is no separate sequence. */
  id: string
  role: 'user' | 'assistant' | 'system' | 'tool'
  content: string
  /** What the agent did on the way to this reply, and in what order it
   *  happened. `parts` is absent on messages stored before the order was
   *  kept; those are read as their text followed by their calls. */
  metadata: { tool_calls?: ToolCallRecord[]; parts?: MessagePart[] }
  /** How many deltas `content` already accounts for. */
  delta_next: number
  model: string | null
  /** On a reply, the user message it answers. */
  replies_to?: string | null
  /** On a user message, the reply that took it mid-turn. */
  absorbed_by?: string | null
  /** On a user message, where the job answering it is. Only the transcript
   *  read fills this; afterwards the events say. */
  job_state?: 'pending' | 'running' | 'succeeded' | 'failed' | 'cancelled' | null
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
  /** The turn failed. `message_id` names the user message it was answering. */
  | { id: string; kind: 'chat.error'; payload: { message: string; message_id?: string } }
  /** A user message was taken into a turn already running, and will be
   *  answered inside that reply rather than getting one of its own. */
  | {
      id: string
      kind: 'chat.absorbed'
      payload: { message_id: string; absorbed_by: string }
    }
  /** A reply is starting over: the pod running it was lost. */
  | { id: string; kind: 'chat.retry'; payload: { message_id: string; replies_to: string } }
  /** The session was named, by a person or by the namer after its first turn. */
  | { id: string; kind: 'session.renamed'; payload: { title: string } }

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

/** Empty means unnamed: the namer fills it in after the first turn. */
export const renameSession = (sessionId: string, title: string) =>
  api<AgentSession>(`/v1/agent-sessions/${sessionId}`, {
    method: 'PATCH',
    body: JSON.stringify({ title }),
  })

/** What an unnamed session is called wherever a name is shown. */
export const UNNAMED_SESSION = 'New Session'
export const sessionName = (session: { title: string } | undefined) =>
  session?.title || UNNAMED_SESSION

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

/**
 * When a message sent during a turn takes effect.
 *
 * `steer` reaches the agent at its next round boundary, redirecting work
 * already under way. `follow_up` waits until the agent would otherwise stop.
 * The server defaults to steering, which is what someone typing mid-turn
 * almost always means: they are reacting to what they can see.
 */
export type Delivery = 'steer' | 'follow_up'

export const sendMessage = (sessionId: string, content: string, delivery?: Delivery) =>
  api<Message>(`/v1/agent-sessions/${sessionId}/messages`, {
    method: 'POST',
    body: JSON.stringify({ content, timezone: timezone(), delivery }),
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

/** A file in one of a conversation's three storage scopes. */
export type StoredFile = {
  /** As the agent names it: `session/report.pdf`. */
  path: string
  scope: 'session' | 'agent' | 'workspace'
  size: number
}

export function listFiles(sessionId: string): Promise<StoredFile[]> {
  return api<StoredFile[]>(`/v1/agent-sessions/${sessionId}/files`)
}

/** Where a file is fetched from; the session cookie travels with the link. */
export function fileUrl(sessionId: string, scopedPath: string): string {
  return `/v1/agent-sessions/${sessionId}/files/${scopedPath}`
}

export async function uploadFile(
  sessionId: string,
  scope: StoredFile['scope'],
  file: File,
): Promise<StoredFile> {
  // Raw bytes, not JSON: the file is the body.
  return api<StoredFile>(fileUrl(sessionId, `${scope}/${encodeURIComponent(file.name)}`), {
    method: 'PUT',
    headers: { 'content-type': file.type || 'application/octet-stream' },
    body: file,
  })
}

export function deleteFile(sessionId: string, scopedPath: string): Promise<void> {
  return api<void>(fileUrl(sessionId, scopedPath), { method: 'DELETE' })
}
