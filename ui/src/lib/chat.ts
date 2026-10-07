import { api, ApiError, allPages } from './api'

export type Agent = {
  id: string
  name: string
  slug: string
  description: string
  enabled: boolean
  /** Whether this reader may start a conversation with it. */
  can_chat: boolean
  /** The operator's template it was made from, if any. */
  template_id?: string | null
}
export type AgentSession = {
  id: string
  agent_id: string
  title: string
  /** When a message was last stored in it; the list is ordered by this. */
  last_active_at?: string
  /** The live turn's job state, absent when nothing is in flight. */
  turn?: 'pending' | 'running' | 'parked' | null
}

/** A tool the agent ran, labeled by the agent with what it was doing. */
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
  /** Where the model stopped to think, and what it thought. Positioned rather
   *  than collected: a model that thinks, calls a tool, reads the answer and
   *  thinks again deliberated twice about two different things. */
  | {
      type: 'reasoning'
      text: string
      /** How long this thought took, first fragment to last. Absent on a
       *  thought recorded before it was measured. */
      ms?: number
      /** This tab's clock when `ms` was last measured, while the thought is
       *  streaming. Never from the server and never stored: it is what lets
       *  the running figure tick between fragments without comparing this
       *  browser's clock with the server's. */
      seenAt?: number
    }
  /** The point a message the user sent mid-turn was handed to the agent,
   *  naming that message. The reply is drawn split here, with the message
   *  between its halves, since what follows is answering it. */
  | { type: 'steer'; id: string }

/** Whether a reply has anything in it, by the one rule everything uses.
 *
 * Three things count, and the third is easy to forget: the words, the calls it
 * made, and the points it stopped to think. A reply holding a thought and
 * nothing else is a turn the model spent deliberating -- the only account of
 * where its tokens went -- so treating it as empty discards it.
 *
 * One function because this was written out twice and the two copies had
 * already drifted: the second omitted the calls, so a failed turn's reply that
 * had made calls vanished from the page while surviving in the transcript, and
 * came back on the next reload.
 *
 * The server applies the same rule in SQL -- `discard_placeholder` and the
 * abandoned-placeholder guard in `src/api/chat/postgres.rs`, and
 * `parts::Builder::said_something` in Rust. Those cannot share this, being in
 * another language; changing the rule means changing all of them.
 */
export function saidSomething(m: Pick<Message, 'content' | 'metadata'>): boolean {
  if (m.content !== '') return true
  if ((m.metadata.tool_calls?.length ?? 0) > 0) return true
  return m.metadata.parts?.some((p) => p.type === 'reasoning') ?? false
}

/** What the platform recorded when an agent woke. See `api::wake`. */
export type WakeRecord = {
  kind: 'sleep' | 'timer'
  reason: string
  set_at: string
  due_at: string
  woke_at: string
  /** Who pressed Wake now, where somebody did. */
  woken_by?: string | null
  /** The messages sent while it slept, which the wake answers together. */
  answers?: string[]
}

/** A conversation whose agent is asleep, and how to end it early. */
export type Asleep = {
  item_id: string
  until: string
  reason: string
}

export type Message = {
  /** UUIDv7: ordering is carried by the id, so there is no separate sequence. */
  id: string
  role: 'user' | 'assistant' | 'system' | 'tool'
  content: string
  /** What the agent did on the way to this reply, and in what order it
   *  happened. `parts` is absent on messages stored before the order was
   *  kept; those are read as their text followed by their calls. */
  metadata: {
    tool_calls?: ToolCallRecord[]
    parts?: MessagePart[]
    /** Present when this message is a compaction summary standing in for the
     *  conversation up to the id it names. Shown as a summary rather than as
     *  something the agent said, so a reader can see that their conversation
     *  was compacted and what it was replaced by. */
    summary_through?: string
    /** Present on a reply whose turn was cut off -- its runtime lost, its
     *  lease reaped -- and started again. What it streamed is kept, because the
     *  reader watched it happen; the attempt after it is the answer. */
    interrupted?: boolean
    /** Present when this message records somebody answering an approval.
     *  Drawn as a boundary rather than as speech: the agent did not say it,
     *  the platform recorded what a person decided. */
    approval?: {
      requires: string
      approved: boolean
      answered_by?: string | null
      answered_by_name?: string | null
      note?: string | null
    }
    /** Present when this message is the note an agent woke to, after a sleep
     *  or a timer. Written by the platform rather than sent by anybody, so it
     *  is drawn as a boundary rather than as somebody's message. */
    wake?: WakeRecord
  }
  /** How many deltas `content` already accounts for. */
  delta_next: number
  model: string | null
  /** On a reply, the user message it answers. */
  replies_to?: string | null
  /** Which attempt at answering its prompt this reply is. */
  attempt?: number
  /** When it stopped being written, as distinct from when it was created.
   *  The id is creation time -- for a reply that waited on an approval, those
   *  are minutes apart. */
  finished_at?: string | null
  /** On a user message, the reply that took it mid-turn. */
  absorbed_by?: string | null
  /** On a user message, where the job answering it is. Only the transcript
   *  read fills this; afterwards the events say. */
  job_state?:
    | 'pending'
    | 'running'
    | 'succeeded'
    | 'failed'
    | 'cancelled'
    | 'parked'
    | null
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
  /** The approval this conversation is waiting on, if it is waiting on one.
   *  Served with the history because a reader who reloads has to learn
   *  everything true *now* from one answer -- the live `chat.held` event is
   *  gone by then. */
  awaiting?: {
    item_id: string
    requires?: string | null
    reason?: string | null
    covers?: { field?: string; unit?: string } | null
  } | null
  /** Whether the agent is asleep, for the same reason. */
  asleep?: Asleep | null
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
  /** A fragment of the model's thinking. No index: it is not part of the
   *  reply, so nothing concatenates it to stored content. */
  | {
      id: string
      kind: 'chat.reasoning'
      payload: { message_id: string; text: string }
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
  /** The model began writing a call to a tool: its name, before any of its
   *  arguments. Shown as a call being prepared until the call itself starts;
   *  never stored. `index` is the call's place among its round's calls. */
  | {
      id: string
      kind: 'chat.writing'
      /** `bytes` is how much of the call's arguments are written; zero, or
       *  absent from an older runtime, when the call has only begun. */
      payload: { message_id: string; index: number; name: string; bytes?: number }
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
  /** A message sent mid-turn was handed to the agent, at this point in the
   *  reply. `id` is the user message; `message_id` the reply it joined. */
  | { id: string; kind: 'chat.steer'; payload: { message_id: string; id: string } }
  | { id: string; kind: 'chat.done'; payload: { message_id: string } }
  /** The turn failed. `message_id` names the user message it was answering. */
  | { id: string; kind: 'chat.error'; payload: { message: string; message_id?: string } }
  /** The turn did not run because something is holding it. Not an error: a
   *  failure is retried and a stop is not, and a reader shown an error for a
   *  deliberate pause is told the system broke. `resumable` is true when the
   *  turn runs again of its own accord once the hold lifts, and false when a
   *  person has to say something to restart the conversation. */
  | {
      id: string
      kind: 'chat.held'
      payload: { message: string; message_id?: string; resumable: boolean }
    }
  /** The agent went to sleep. Nothing it is sent is answered until it wakes,
   *  which it does at `until` or when somebody presses Wake now. */
  | { id: string; kind: 'chat.sleeping'; payload: Asleep }
  /** A user message was taken into a turn already running, and will be
   *  answered inside that reply rather than getting one of its own. */
  | {
      id: string
      kind: 'chat.absorbed'
      payload: { message_id: string; absorbed_by: string }
    }
  /** A reply is starting over: the pod running it was lost. */
  | { id: string; kind: 'chat.retry'; payload: { message_id: string; replies_to: string } }
  /** A failed turn was put back on the queue, because somebody asked for it.
   *  Distinct from `chat.retry`, which is the platform starting a turn over
   *  by itself after losing the runtime: that one needs no button and says
   *  so in the mark, while this one follows a person pressing one. */
  | {
      id: string
      kind: 'chat.requeued'
      payload: { message_id: string; job_id: string }
    }
  /** A summary of the conversation is being written; `chat.compacted` follows,
   *  saying whether one was. The system prompt is composed again either way. */
  | { id: string; kind: 'chat.compacting'; payload: Record<string, never> }
  | { id: string; kind: 'chat.compacted'; payload: { summarized: boolean } }
  /** The session was named, by a person or by the namer after its first turn. */
  | { id: string; kind: 'session.renamed'; payload: { title: string } }

type PollResponse = {
  events: ChatEvent[]
  cursor: string
}

export const listAgents = () => allPages<Agent>('/v1/agents')

/** One agent's turns in flight and when it was last active. */
export type AgentActivity = {
  agent_id: string
  running: number
  queued: number
  waiting: number
  last_active_at: string | null
  live_session_id: string | null
}
export const agentActivity = () => api<AgentActivity[]>('/v1/agents/activity')
/** One page of the recent list, most recently active first. `next` continues
 *  it; `q` narrows it to titles containing those words, in the same order. */
export type SessionPage = { items: AgentSession[]; next: string | null }

export function sessionsPage(opts: { after?: string; q?: string } = {}): Promise<SessionPage> {
  const params = new URLSearchParams({ limit: '50' })
  if (opts.after) params.set('after', opts.after)
  if (opts.q) params.set('q', opts.q)
  return api<SessionPage>(`/v1/agent-sessions?${params}`)
}

/** One session, for a link to a conversation the paged list has not reached. */
export const getSession = (sessionId: string) =>
  api<AgentSession>(`/v1/agent-sessions/${sessionId}`)

/** One agent's most recently active sessions. */
export const recentSessionsOf = async (agentId: string, limit: number) =>
  (await api<{ items: AgentSession[] }>(`/v1/agent-sessions?agent=${agentId}&limit=${limit}`)).items

/** The most recently active sessions: one page, never the whole history. */
export const recentSessions = async () =>
  (await api<{ items: AgentSession[] }>('/v1/agent-sessions?limit=50')).items

/**
 * A fresh first page laid over the list already held.
 *
 * Anything that became active since the last read is on the first page,
 * because the list is ordered by last activity -- and that includes a turn
 * finishing, which stores its reply and so touches the session. So the page
 * replaces what it covers and everything older is kept as it was, which keeps
 * the poll the size of a page however long the workspace has existed.
 */
export function mergeRecent(held: AgentSession[], fresh: AgentSession[]): AgentSession[] {
  const seen = new Set(fresh.map((s) => s.id))
  const merged = [...fresh, ...held.filter((s) => !seen.has(s.id))]
  // Stable, so sessions with no timestamp keep the order they arrived in.
  return merged.sort((a, b) => (b.last_active_at ?? '').localeCompare(a.last_active_at ?? ''))
}

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

/** What asking a turn to stop achieved. */
export type Cancelled = {
  /** False when there was nothing in flight to stop. */
  stopped: boolean
  /** 'cancelled' if it had not started, 'stopping' if it was running, or
   *  'nothing_running' if it had already finished on its own. */
  state: 'cancelled' | 'stopping' | 'nothing_running'
}

/**
 * Asks the turn this session has in flight to stop.
 *
 * Answers what it did rather than only that it worked, because the cases feel
 * different: a turn that had not started is over at once, a running one takes
 * until its next round boundary, and one that finished while the button was
 * being pressed was never stopped at all.
 */
export const cancelTurn = (sessionId: string) =>
  api<Cancelled>(`/v1/agent-sessions/${sessionId}/cancel`, { method: 'POST' })

/** A skill whose version moved on since the conversation's prompt was
 *  composed. `kept` is null for one added since, `live` for one removed. */
export type StaleSkill = { name: string; kept: number | null; live: number | null }

/** Whether a conversation's kept system prompt is what would be composed now. */
export type PromptStatus = {
  composed_at: string | null
  current: boolean
  skills: StaleSkill[]
}

export const getPromptStatus = (sessionId: string) =>
  api<PromptStatus>(`/v1/agent-sessions/${sessionId}/prompt`)

/** Queues a compaction, which composes the prompt again from what is live. */
export const compactSession = (sessionId: string) =>
  api<void>(`/v1/agent-sessions/${sessionId}/compact`, { method: 'POST' })

/**
 * Ends the agent's sleep now.
 *
 * `woke` is false when it was already awake -- somebody else pressed it, or the
 * time came -- which is not an error: either way it is awake.
 */
export const wakeSession = (sessionId: string) =>
  api<{ woke: boolean }>(`/v1/agent-sessions/${sessionId}/wake`, { method: 'POST' })

/** What asking for a failed turn to run again achieved. */
export type Retried = {
  queued: boolean
  state: 'queued' | 'not_failed' | 'no_turn'
}

/**
 * Runs a failed turn again.
 *
 * The message is already stored and the turn that failed is a job against it,
 * so this requeues that job. Sending the text a second time would ask the
 * agent the same thing twice, which is what the retry button used to do by
 * putting the words back in the composer while the failed message stayed in
 * the transcript.
 */
export const retryTurn = (sessionId: string, messageId: string) =>
  api<Retried>(`/v1/agent-sessions/${sessionId}/messages/${messageId}/retry`, {
    method: 'POST',
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

/** A page of a conversation's files, and where each scope with more goes on. */
export type FilePage = {
  items: StoredFile[]
  /** The cursor to continue each scope that has more, by scope. */
  more: Partial<Record<StoredFile['scope'], string>>
}

/** The first page of every scope, or with `scope` and `after`, the next page
 *  of one. Paged per scope, because they are three listings in the store. */
export function listFiles(
  sessionId: string,
  scope?: StoredFile['scope'],
  after?: string,
): Promise<FilePage> {
  const query = scope
    ? `?scope=${scope}${after ? `&after=${encodeURIComponent(after)}` : ''}`
    : ''
  return api<FilePage>(`/v1/agent-sessions/${sessionId}/files${query}`)
}

/** Where a file is fetched from; the session cookie travels with the link. */
export function fileUrl(sessionId: string, scopedPath: string): string {
  return `/v1/agent-sessions/${sessionId}/files/${scopedPath}`
}

/** Where a file can be looked at rather than downloaded. */
export function previewUrl(sessionId: string, scopedPath: string): string {
  return `/v1/agent-sessions/${sessionId}/files/preview/${scopedPath}`
}

/** What a preview turned out to be. */
export type Preview =
  | { kind: 'text'; text: string; truncated: boolean }
  | { kind: 'image'; url: string }
  | { kind: 'none' }

/**
 * Fetches a file for looking at.
 *
 * The server decides what a file is, from its bytes, and refuses anything
 * outside a short allowlist -- so this trusts the `content-type` it is given
 * and never the path. A name is a claim by whoever uploaded it.
 *
 * An image comes back as a blob URL rather than being pointed at directly,
 * because the preview endpoint needs the session cookie and an `img` tag
 * carrying credentials is a different conversation. The caller revokes it.
 */
export async function readPreview(
  sessionId: string,
  scopedPath: string,
): Promise<Preview> {
  const response = await fetch(previewUrl(sessionId, scopedPath), {
    credentials: 'include',
  })
  if (response.status === 415) return { kind: 'none' }
  if (!response.ok) {
    throw new ApiError(response.status, await response.text())
  }

  const type = response.headers.get('content-type') ?? ''
  const truncated = response.headers.get('x-outturn-truncated') === 'true'

  if (type.startsWith('image/')) {
    return { kind: 'image', url: URL.createObjectURL(await response.blob()) }
  }
  return { kind: 'text', text: await response.text(), truncated }
}

/** Whether a path is one the markdown renderer should handle. */
export function isMarkdown(path: string): boolean {
  return /\.(md|markdown)$/i.test(path)
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

/**
 * Stores something pasted into the composer.
 *
 * A pasted image is a `Blob` off the clipboard rather than a `File` off a
 * disk: it has bytes and a type and no name at all. So a name is made here,
 * from the clock, which also keeps two pastes in one conversation from
 * landing on top of each other -- `image.png` twice would mean the second
 * silently replacing the first.
 */
export async function uploadPastedImage(
  sessionId: string,
  blob: Blob,
): Promise<StoredFile> {
  const extension = extensionFor(blob.type)
  // Sortable, unambiguous, and readable in a file list: pasted-20260915-081530.png
  const stamp = new Date()
    .toISOString()
    .replace(/[-:]/g, '')
    .replace(/\.\d+Z$/, '')
    .replace('T', '-')
  const name = `pasted-${stamp}.${extension}`

  return api<StoredFile>(fileUrl(sessionId, `session/${encodeURIComponent(name)}`), {
    method: 'PUT',
    headers: { 'content-type': blob.type || 'application/octet-stream' },
    body: blob,
  })
}

/** The file extension for a clipboard image, by what the browser called it. */
function extensionFor(mediaType: string): string {
  switch (mediaType) {
    case 'image/png':
      return 'png'
    case 'image/jpeg':
      return 'jpg'
    case 'image/gif':
      return 'gif'
    case 'image/webp':
      return 'webp'
    default:
      // Safari has been known to paste `image/tiff`. Stored under its own
      // name anyway: the host reads the bytes to decide what it is, so a
      // wrong guess here costs nothing, and refusing the paste outright
      // would lose something the user meant to keep.
      return mediaType.split('/')[1]?.replace(/[^a-z0-9]/gi, '') || 'bin'
  }
}

export function deleteFile(sessionId: string, scopedPath: string): Promise<void> {
  return api<void>(fileUrl(sessionId, scopedPath), { method: 'DELETE' })
}
