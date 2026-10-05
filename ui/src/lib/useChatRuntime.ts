import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { mintedAt } from './elapsed'
import {
  useExternalStoreRuntime,
  type AppendMessage,
  type ThreadMessageLike,
} from '@assistant-ui/react'

import {
  cancelTurn,
  loadHistory,
  pollEvents,
  retryTurn,
  saidSomething as hasContent,
  sendMessage,
  type Asleep,
  type Delivery,
  type Message,
  type MessagePart,
  type ToolCallRecord,
} from './chat'

/**
 * Where a user's message is in its life, for the reader.
 *
 * Derived from what the system actually reports rather than assumed: a
 * message is queued until a runtime takes it, waiting on the model until the
 * first token, and so on. Null once the reply is visibly underway -- the
 * reply then speaks for itself.
 */
export type MessageStatus =
  /** Stored, no runtime has taken it yet. */
  | { kind: 'queued' }
  /** Sent while a reply was being written; it will join that reply at the
   *  agent's next step rather than wait for a turn of its own. */
  | { kind: 'steering' }
  /** A runtime has it and the model has been asked; nothing back yet. */
  | { kind: 'waiting' }
  /** The pod running it was lost and the turn is starting over. */
  | { kind: 'retrying' }
  /** Taken into a turn already running; answered there, not separately. */
  | { kind: 'absorbed' }
  /** Paused by a hold -- an approval, a spend cap -- and waiting to carry on.
   *  The `chat.held` event says this while the reader is watching; this is how
   *  a reload says it too, since the event is long gone by then. */
  | { kind: 'held' }
  /** The turn finished and the reply is empty: the agent said nothing. Not a
   *  failure -- nothing went wrong that anyone recorded -- but the reader is
   *  owed an explanation rather than a spinner that never stops. */
  | { kind: 'silent' }
  | { kind: 'failed'; message: string }

/** An approval waiting on somebody, as the held event named it. */
export type PendingApproval = {
  item_id: string
  requires?: string | null
  reason?: string | null
  covers?: { field?: string; unit?: string } | null
  /** Each field the grant is keyed on, with this request's value. */
  binds?: { field: string; value: unknown }[] | null
}

export type Annotated = Message & {
  status?: MessageStatus | null
  /** On a reply: its turn is still running, so a call without a result is
   *  one still being run, not one whose turn died before it answered. */
  live?: boolean
  /** The newest assistant reply in the transcript: the only one that wears
   *  the finished mark, and the reason it is still there after a refresh. */
  newest?: boolean
  /** On an interrupted reply: a later attempt at the same prompt said
   *  something, or is still being written. What lets the thread promise the
   *  agent "started again below" only when there is something below. */
  restarted?: boolean
}

/**
 * Works out each user message's status from the transcript around it.
 *
 * `retrying` is the one thing the transcript cannot tell: a retry reuses the
 * same empty reply, so it is remembered from the event until a delta arrives.
 */
/**
 * A message with whatever was quoted into it, as markdown.
 *
 * The composer carries a quoted passage in its metadata until send, and this
 * is where it becomes part of what is actually sent. Written as an ordinary
 * `>` block rather than as a structure: the model already knows what that
 * means, the transcript reads back the way it was written, and nothing new
 * has to be honoured at either end. A richer shape can replace this later
 * without the quote having gone missing in the meantime.
 *
 * Every line is prefixed, not just the first. A multi-paragraph selection
 * with one `>` on it is a quote for exactly one line and ordinary text
 * afterwards, which reads as the person having said the rest themselves.
 */
export function withQuote(text: string, custom: unknown): string {
  const quote = (custom as { quote?: { text?: string } } | undefined)?.quote?.text
  if (!quote?.trim()) return text
  const block = quote
    .trim()
    .split('\n')
    .map((line) => `> ${line}`)
    .join('\n')
  return `${block}\n\n${text}`
}

/** Whether a turn is running in this session, from the transcript alone.
 *
 * Derived rather than remembered, because the thing it drives has to be true
 * for whoever is looking. `isRunning` used to be set only by this tab sending,
 * retrying or requeueing -- so somebody watching a colleague's session was
 * offered no stop button, and a message they typed mid-reply was enqueued as a
 * new turn rather than steered into the running one. The library picks between
 * `enqueue` and `steer` on the same flag, so both followed from one gap.
 *
 * Named states only, and only the two that mean work is outstanding. Every
 * terminal state -- `succeeded`, `failed`, `cancelled` -- and `parked`, which is
 * a turn waiting on a person rather than on a model, must read as not running,
 * or the composer offers stop for ever and every later message becomes a steer
 * into nothing. `job_state` is also null in the window between storing a
 * message and enqueueing its turn, which reads as not running and corrects
 * itself on the next poll.
 */
export function turnIsRunning(messages: Message[]): boolean {
  return messages.some(
    (m) => m.role === 'user' && (m.job_state === 'pending' || m.job_state === 'running'),
  )
}

export function annotate(
  messages: Message[],
  retrying: Set<string>,
  failures: Map<string, string>,
): Annotated[] {
  // The latest attempt at answering each prompt, and whether *any* attempt
  // said something.
  //
  // A prompt can have more than one reply now: a turn stopped for an approval
  // keeps what it wrote and the resumed turn writes a new attempt beside it.
  // Keyed on `replies_to` alone, the last one won -- so a prompt whose first
  // attempt made four tool calls and said its piece was judged by an empty
  // second attempt and drawn as "the agent did not reply", directly above the
  // reply it had in fact given.
  const replyFor = new Map<string, Message>()
  const answered = new Set<string>()
  for (const m of messages) {
    if (m.role === 'assistant' && m.replies_to) {
      replyFor.set(m.replies_to, m)
      // Thinking counts. It is not an answer, but it is visibly the agent at
      // work with its own streaming indicator -- and a prompt that still says
      // "waiting" above it puts two spinners on screen saying different things
      // about the same turn.
      if (hasContent(m)) answered.add(m.replies_to)
    }
  }


  // Replies still being written, by id. A message folded into one of these
  // is worth pointing out while it is happening; once the reply is done the
  // transcript speaks for itself, as it does for every other message.
  const inProgress = new Set(
    messages
      .filter(
        (m) =>
          m.role === 'user' &&
          replyFor.has(m.id) &&
          (m.job_state === 'pending' || m.job_state === 'running'),
      )
      .map((m) => replyFor.get(m.id)!.id),
  )

  // Whether a reply is being written right now: some prompt has a reply
  // and its job has not finished. A message queued behind that is not
  // waiting for a runtime, it is waiting for the agent's next step, and
  // saying "queued" to someone who just typed mid-reply reads as a fault.
  const replyInProgress = inProgress.size > 0

  // The last assistant message, by the order the transcript is already in --
  // ids are UUIDv7, so the newest is simply the last one. Derived here rather
  // than remembered by a component: a component only knows what it watched,
  // so every reply it ever saw finish would keep its mark, and a refresh
  // would clear the lot.
  const newest = [...messages].reverse().find((m) => m.role === 'assistant')?.id

  // An attempt after an interrupted one, worth pointing the reader at: it
  // said something, or it has not finished yet and may. An empty finished
  // retry is not -- one ran to the end having said nothing, and the note
  // above the interrupted attempt sent the reader looking for a reply that
  // was not there.
  const restarted = (m: Message) =>
    messages.some(
      (later) =>
        later.role === 'assistant' &&
        later.replies_to === m.replies_to &&
        (later.attempt ?? 1) > (m.attempt ?? 1) &&
        (hasContent(later) || inProgress.has(later.id) || !later.finished_at),
    )

  return messages.map((m) => {
    if (m.role === 'assistant') {
      return {
        ...m,
        live: inProgress.has(m.id),
        newest: m.id === newest,
        restarted: m.metadata.interrupted === true && restarted(m),
      }
    }
    if (m.role !== 'user') return m

    if (m.absorbed_by) {
      return { ...m, status: inProgress.has(m.absorbed_by) ? { kind: 'absorbed' } : null }
    }

    const reply = replyFor.get(m.id)
    // Any attempt saying something is the prompt being answered. Judging only
    // the latest calls a prompt unanswered the moment a resumed turn opens an
    // empty reply beside the one that answered it.
    if (answered.has(m.id)) return { ...m, status: null }

    const failure = failures.get(m.id)
    if (failure !== undefined || m.job_state === 'failed') {
      return { ...m, status: { kind: 'failed', message: failure ?? 'the turn failed' } }
    }

    if (reply) {
      // A reply exists but has nothing in it. While the job is still going
      // that is a turn yet to say its first word; once the job has finished it
      // is a turn that ended without saying anything, and calling that
      // "waiting" leaves the reader watching a spinner for a reply that is
      // never coming. Small models do this with thinking off, and any model does
      // it by spending its whole turn on tool calls that go nowhere.
      // Named states only. `job_state` is null while no job row exists for
      // the message yet -- the window between storing it and enqueueing its
      // turn, which a poll lands in often enough to see -- and reading that
      // as "over" declared the agent silent moments before it started
      // streaming, next to a stop button saying the opposite.
      //
      // `failed` is not among them because it returned above; the compiler
      // says so, which is how a fourth state written here out of symmetry
      // was caught.
      const jobOver = m.job_state === 'succeeded' || m.job_state === 'cancelled'
      if (jobOver && !retrying.has(reply.id)) {
        return { ...m, status: { kind: 'silent' } }
      }
      // Parked before `waiting`, because a parked turn has exactly the shape
      // `waiting` is for -- a reply with nothing in it -- and nothing is
      // working on it. Left out, a reader who reloads while an approval is
      // pending watches a spinner for the model, which is not what is being
      // waited on.
      if (m.job_state === 'parked') {
        return { ...m, status: { kind: 'held' } }
      }
      return { ...m, status: { kind: retrying.has(reply.id) ? 'retrying' : 'waiting' } }
    }

    switch (m.job_state) {
      case 'pending':
        return { ...m, status: { kind: replyInProgress ? 'steering' : 'queued' } }
      case 'running':
        return { ...m, status: { kind: 'waiting' } }
      case 'parked':
        return { ...m, status: { kind: 'held' } }
      default:
        // Answered long ago, or nothing was ever queued for it. Either way
        // there is nothing to report.
        return { ...m, status: null }
    }
  })
}

/**
 * Our stored message, mapped to what assistant-ui renders.
 *
 * The id is carried through deliberately: it is what lets a message that
 * arrives over the event feed replace the one already on screen rather than
 * appear beside it. Deltas append to the content of this same id, so there is
 * never a separate "streaming" object to swap in.
 *
 * The status rides in `metadata.custom`, which is where assistant-ui lets an
 * application attach its own facts to a message for its components to read.
 */
const convertMessage = (message: Annotated): ThreadMessageLike => ({
  id: message.id,
  role: message.role === 'tool' ? 'assistant' : message.role,
  metadata: {
    custom: {
      status: message.status ?? null,
      // Whether this reply's turn is still going, from its job. assistant-ui's
      // own `running` is only ever on the last message in the thread, so a
      // message queued below a streaming reply took it away and the reply's
      // mark settled into its finished line mid-stream.
      live: message.live === true,
      // A compaction summary is shown as one. It is stored as an assistant
      // message because that is what it is -- a model wrote it -- but it was
      // never said to the reader, and drawn as ordinary speech it reads as the
      // agent summarising the conversation back at them unprompted. Marked
      // here so the thread can draw it as the boundary it is: everything above
      // it is what the agent now remembers of what came before.
      summary: message.metadata.summary_through != null,
      // A reply cut off partway and started again. Kept rather than replaced,
      // because the reader watched it happen -- but drawn as ordinary speech
      // with nothing to say it stopped, it reads as an answer followed by a
      // second, unrelated one.
      interrupted: message.metadata.interrupted === true,
      restarted: message.restarted === true,
      // What somebody decided about an approval, drawn as the boundary it is
      // rather than as speech. Stored as a `system` message because the
      // platform recorded it and the agent did not say it -- and without it a
      // reader who reloads sees a refused tool call followed by a success with
      // nothing joining them, the pause and the decision having left no trace.
      approval: message.metadata.approval ?? null,
      // The note an agent woke to, which nobody sent. Drawn as a boundary for
      // the same reason: in a bubble on the right it reads as the reader
      // telling the agent it had been asleep.
      wake: message.metadata.wake ?? null,
      // When it stopped being written. The id is creation time, which for a
      // reply that waited on an approval is not remotely the same thing.
      finishedAt: message.finished_at ?? null,
      // Only the newest reply wears the finished mark. Every other one is
      // just conversation, and a line under each would be noise by the
      // twentieth turn.
      newest: message.newest === true,
    },
  },
  content: parts(message),
})

/**
 * A reply in the order it was produced.
 *
 * Not tools-then-text: an agent asked to say what it is about to do says it
 * first, and drawing the call above those words shows a turn that never
 * happened. The order is recorded on the message; a message stored before it
 * was kept is read the way it used to be replayed -- its calls, then its
 * words -- which is what those turns were.
 */
export function parts(message: Annotated): ThreadMessageLike['content'] {
  const calls = message.metadata.tool_calls ?? []
  const drawn = (call: ToolCallRecord) => ({
    type: 'tool-call' as const,
    toolCallId: call.id,
    toolName: call.name,
    // The action is the model's own account of what it is doing, and the
    // only argument the user is shown.
    // Absent rather than undefined: the part must be plain JSON, and an
    // explicit undefined is not.
    args: {
      action: call.action,
      details: call.details ?? '',
      isError: call.is_error ?? false,
      // Announced but not yet answered, on a turn still running. A call
      // with no result on a finished turn is not pending; it is a turn that
      // ended before the tool did, and spinning for ever would say otherwise.
      pending: call.details === undefined && message.live === true,
    },
    argsText: JSON.stringify({ action: call.action }),
  })

  const recorded = message.metadata.parts
  if (!recorded || recorded.length === 0) {
    return [
      ...calls.map(drawn),
      { type: 'text' as const, text: message.content },
    ] as ThreadMessageLike['content']
  }

  const out = []
  for (const part of recorded) {
    if (part.type === 'text') {
      if (part.text !== '') out.push({ type: 'text' as const, text: part.text })
      continue
    }
    // In place, so a thought about a tool result is drawn after the call it is
    // about rather than hoisted to the top of the reply as though the model had
    // known the answer before it asked.
    if (part.type === 'reasoning') {
      if (part.text !== '') {
        out.push({
          type: 'reasoning' as const,
          text: part.text,
          // assistant-ui hands a part's own fields to its component, so the
          // duration rides along with the text it belongs to.
          ...(part.ms === undefined ? {} : { ms: part.ms }),
          ...(part.seenAt === undefined ? {} : { seenAt: part.seenAt }),
        })
      }
      continue
    }
    const call = calls.find((c) => c.id === part.id)
    if (call) out.push(drawn(call))
  }
  // A call that arrived after the parts were written -- the live stream adds
  // to both, and the message can be read between the two.
  for (const call of calls) {
    if (!recorded.some((p) => p.type === 'call' && p.id === call.id)) out.push(drawn(call))
  }
  return out as ThreadMessageLike['content']
}

/**
 * Draws a reply as separate boxes wherever a message was handed to it
 * mid-turn, with that message between them.
 *
 * One turn writes one reply, however many rounds it takes -- so a message the
 * agent took at a round boundary is stored as a single reply that answers it,
 * and sorted by id the message lands below that reply. What the reader should
 * see is what happened: the reply so far, their message, and the rest as the
 * answer to it. Only the drawing changes. The stored reply, and what the
 * model is sent next turn, stay exactly as they were.
 *
 * Later boxes get ids derived from the reply's, which keep its timestamp.
 * Only the last one wears the mark and the live state, since it is the one
 * still being written.
 */
export function splitAtSteers(messages: Annotated[]): Annotated[] {
  const byMessage = new Map(messages.map((m) => [m.id, m]))
  // Only where the message is on this page; a split with nothing to show
  // between its halves would be a break with no reason given.
  const steerOf = (part: MessagePart) =>
    part.type === 'steer' && byMessage.get(part.id)?.role === 'user' ? part.id : null
  const placed = new Set(
    messages.flatMap((m) =>
      m.role === 'assistant' ? (m.metadata.parts ?? []).map(steerOf).filter((id) => id !== null) : [],
    ),
  )
  if (placed.size === 0) return messages

  const out: Annotated[] = []
  for (const m of messages) {
    if (placed.has(m.id)) continue
    const parts = m.role === 'assistant' ? m.metadata.parts : undefined
    if (!parts || !parts.some((p) => steerOf(p) !== null)) {
      out.push(m)
      continue
    }

    const segments: { parts: MessagePart[]; before: string | null }[] = [
      { parts: [], before: null },
    ]
    for (const part of parts) {
      const steer = steerOf(part)
      if (steer !== null) segments.push({ parts: [], before: steer })
      else if (part.type !== 'steer') segments[segments.length - 1].parts.push(part)
    }

    const calls = m.metadata.tool_calls ?? []
    const inAnyPart = (id: string) => parts.some((p) => p.type === 'call' && p.id === id)
    segments.forEach((segment, i) => {
      const last = i === segments.length - 1
      if (segment.before !== null) {
        // Shown where it was taken, so it needs no badge saying so. But until
        // the box after it has something in it, that box is not drawn, and
        // nothing on screen would say the agent is working on it -- so the
        // message says what a fresh prompt would.
        const answering = last && m.live === true && segment.parts.length === 0
        out.push({
          ...byMessage.get(segment.before)!,
          status: answering ? { kind: 'waiting' } : null,
        })
      }
      // The host streams a blank line before a later round's first words.
      // It separated rounds in one box; at the top of a new one it is space.
      const firstText = segment.parts.findIndex((p) => p.type === 'text')
      const own = segment.parts.map((p, j) =>
        i > 0 && j === firstText && p.type === 'text'
          ? { ...p, text: p.text.replace(/^\s+/, '') }
          : p,
      )
      out.push({
        ...m,
        id: i === 0 ? m.id : `${m.id}:${i}`,
        content: own.map((p) => (p.type === 'text' ? p.text : '')).join(''),
        metadata: {
          ...m.metadata,
          parts: own,
          // Each box draws its own calls. A call not yet in the parts -- read
          // between the two live updates -- belongs to the one being written.
          tool_calls: calls.filter(
            (c) =>
              own.some((p) => p.type === 'call' && p.id === c.id) || (last && !inAnyPart(c.id)),
          ),
        },
        newest: last && m.newest,
        live: last && m.live,
      })
    })
  }
  return out
}

/** Ids are UUIDv7, so lexical order is insertion order. */
const byId = (a: Message, b: Message) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)

/** Where the next delta belongs, per message id. */
type DeltaProgress = Map<string, number>

/**
 * `onRenamed` is told the session's new title whenever the feed says it
 * changed -- the namer working after a turn, or somebody else's rename --
 * so the sidebar and header follow without a reload.
 */
/** Marks a call card shown while its call is still being written. */
const PREPARING = 'preparing:'

/** A message without the cards of calls that never started. */
function withoutPreparing(m: Message): Message {
  const calls = m.metadata.tool_calls ?? []
  if (!calls.some((c) => c.id.startsWith(PREPARING))) return m
  return {
    ...m,
    metadata: {
      ...m.metadata,
      tool_calls: calls.filter((c) => !c.id.startsWith(PREPARING)),
      parts: (m.metadata.parts ?? []).filter(
        (p) => !(p.type === 'call' && p.id.startsWith(PREPARING)),
      ),
    },
  }
}

/**
 * The tools that store or remove a file, whose results mean a files view is out
 * of date. Listed by name rather than inferred, since a tool that only reads a
 * file finishing says nothing changed.
 */
const FILE_TOOLS = new Set(['write_object', 'delete_object', 'expand_archive', 'create_archive'])

/** Why a conversation is not running, when something is holding it. */
type Held = {
  message: string
  resumable: boolean
  /** The approval waiting on somebody, where the hold is one. Absent for a
   *  spend cap or an operator's stop, neither of which is answerable here. */
  approval?: PendingApproval
  /** The sleep, where the hold is one: the agent asked for it, and a person
   *  can end it early. */
  asleep?: Asleep
}

export function useChatRuntime(
  sessionId: string | null,
  onRenamed?: (title: string) => void,
  /** Called as a message is sent, for anything the composer has attached to
   *  it. Returns the text to append and forgets what it returned: an image
   *  reaches the model as a path it can look at, and belongs in the message
   *  rather than travelling beside it, so the transcript records what was
   *  actually asked. */
  takeAttachments?: () => string,
  /** For a conversation that does not exist yet: makes it on the first send,
   *  and is told once the message is in it. Deferred so that picking an agent
   *  and thinking better of it leaves nothing behind in anybody's history. */
  fresh?: {
    /** Who the new chat is with. What says a send is still for the page on
     *  screen: compared by value, so a parent re-rendering does not read as
     *  the reader having left. */
    agent: string
    create: () => Promise<string>
    opened: (id: string) => void
  },
  /** Told when the agent has stored or removed a file, so a files view can
   *  show it. The agent writes straight to the object store, which the API
   *  never sees, so a finished file tool is the only word that anything moved. */
  onFilesChanged?: () => void,
) {
  // Kept in a ref so the feed's effects can reach the latest callback without
  // listing it as a dependency and tearing the stream down on every render.
  // Written in a layout effect rather than during render: a render may be
  // discarded, and a discarded render must not leave a ref behind it.
  const renamed = useRef(onRenamed)
  const attachments = useRef(takeAttachments)
  const starting = useRef(fresh)
  const filesChanged = useRef(onFilesChanged)
  /** Which conversation is on screen now, for a send that outlives it. */
  const showing = useRef(sessionId)
  useLayoutEffect(() => {
    renamed.current = onRenamed
    attachments.current = takeAttachments
    starting.current = fresh
    filesChanged.current = onFilesChanged
    showing.current = sessionId
  })
  const [messages, setMessages] = useState<Message[]>([])
  /** Set by this tab the moment it sends, so the composer answers the click
   *  that caused it rather than waiting for a poll to confirm what we just
   *  did. Not the whole answer: see `isRunning` below. */
  const [sending, setSending] = useState(false)
  /** A stop has been asked for and the turn has not ended yet.
   *
   *  Separate from `isRunning` because the gap between them is real: the turn
   *  stops at its next round boundary, so it is still running, and saying
   *  otherwise would leave the composer offering to stop something already
   *  stopping. */
  const [stopping, setStopping] = useState(false)
  /** A summary is being written, between `chat.compacting` and
   *  `chat.compacted`. */
  const [compacting, setCompacting] = useState(false)
  /** Compactions seen, so what depends on the kept prompt can ask again. */
  const [compactions, setCompactions] = useState(0)
  const [error, setError] = useState<string | null>(null)
  /** Why this conversation is not running, when something is holding it.
   *  Separate from `error` because a hold is not a failure: the turn was not
   *  lost, it was declined, and the reply the reader is waiting for arrives
   *  when the hold lifts or when they say something again. */
  //
  // Kept with the conversation it is about, and shown only on that one. It is
  // set from several places, some of them asynchronous, and was once cleared
  // only by the next conversation's history arriving -- which a new chat never
  // loads -- so an approval card followed the reader out of its conversation
  // and sat over whatever they opened next. Tied to its conversation, it
  // cannot appear on another whatever order the updates land in.
  const [heldAt, setHeldAt] = useState<{ session: string; hold: Held } | null>(null)
  const held = heldAt && heldAt.session === sessionId ? heldAt.hold : null
  const clearHeld = useCallback(() => setHeldAt(null), [])
  // The reader saying they have seen it. Only the notice goes: the failed
  // reply keeps its own mark and its retry, which are what the notice was
  // pointing at.
  const dismissError = useCallback(() => setError(null), [])

  const deltaProgress = useRef<DeltaProgress>(new Map())
  /** Which tool each call in flight is, by call id. A result names only the
   *  call it answers, and whether it moved a file depends on the tool. */
  const callTools = useRef<Map<string, string>>(new Map())
  /** When the thought each reply is writing now began, from its first
   *  fragment's id. The browser's half of `parts::Builder::thinking_since`. */
  const thinkingSince = useRef<Map<string, number>>(new Map())
  /** Replies known to be starting over, until their first delta. */
  const [retrying, setRetrying] = useState<Set<string>>(() => new Set())
  /** Why a user message's turn failed, by user message id. */
  const [failures, setFailures] = useState<Map<string, string>>(() => new Map())

  const merge = useCallback((incoming: Message[]) => {
    if (incoming.length === 0) return
    setMessages((prev) => {
      const known = new Set(prev.map((m) => m.id))
      const added = incoming.filter((m) => !known.has(m.id))
      if (added.length === 0) return prev
      return [...prev, ...added].sort(byId)
    })
  }, [])

  /**
   * Loading history and following the feed are one effect, not two.
   *
   * They were separate, and the poll opened at the start of the log while the
   * history request was still in flight. Every historical delta was then
   * replayed onto content that already contained it, so a reloaded session
   * showed its own text twice. The load now hands the poll the cursor it was
   * read at, and the poll cannot start before that.
   */
  // A compaction belongs to the session it ran in: its progress and its count
  // reset when the conversation changes, so "Compacting…" cannot carry over to
  // the next one. Keyed on the session alone, apart from the poll effect below
  // whose `merge` dependency would otherwise fire this mid-compaction.
  useEffect(() => {
    setCompacting(false)
    setCompactions(0)
  }, [sessionId])

  useEffect(() => {
    if (!sessionId) {
      deltaProgress.current = new Map()
      // Deferred rather than synchronous, so this does not cascade a render.
      queueMicrotask(() => {
        setMessages([])
        setRetrying(new Set())
        setFailures(new Map())
      })
      return
    }

    const controller = new AbortController()
    let stopped = false
    /** What is holding this conversation, recorded as this conversation's. */
    const setHeld = (hold: Held | null) => setHeldAt(hold ? { session: sessionId, hold } : null)

    /** Replaces everything with a fresh snapshot, cursor included. */
    const reload = async (): Promise<string> => {
      const history = await loadHistory(sessionId)
      if (stopped) return history.cursor
      deltaProgress.current = new Map(
        history.messages.map((m) => [m.id, m.delta_next]),
      )
      setMessages(history.messages)
      // The snapshot carries the durable state; anything remembered from
      // events belongs to the stream it came from.
      setRetrying(new Set())
      setFailures(new Map())
      // Whether a turn is in flight is a fact about the transcript rather than
      // about this tab having sent something -- which `isRunning` now reads
      // directly out of `messages` through `turnIsRunning`, on every render
      // rather than only when a snapshot lands. So there is nothing to set
      // here: this was the same derivation, done once and then left to go
      // stale until the next reload.
      //
      // The local flag is cleared instead. It exists only to cover the moment
      // between this tab sending and the first poll that reports the job, and
      // a snapshot is that poll.
      setSending(false)
      // And what it is waiting on, which is durable in exactly the same sense.
      // Driven only by the live `chat.held` event, a tab that was not open when
      // the turn parked -- or was reloaded after -- showed a conversation that
      // had simply stopped, with the question nowhere on screen and no way to
      // answer it.
      if (history.awaiting) {
        setHeld({
          message: 'Paused: this needs somebody to approve it',
          resumable: true,
          approval: history.awaiting,
        })
      } else if (history.asleep) {
        setHeld({ message: 'Asleep', resumable: true, asleep: history.asleep })
      } else {
        setHeld(null)
      }
      return history.cursor
    }

    void (async () => {
      let cursor: string
      try {
        cursor = await reload()
        if (stopped) return
        setStopping(false)
      } catch {
        if (!stopped) setMessages([])
        return
      }

      while (!stopped) {
        try {
          const result = await pollEvents(sessionId, cursor, controller.signal)
          if (stopped) return
          cursor = result.cursor

          merge(
            result.events
              .filter((e) => e.kind === 'chat.message')
              .map((e) => e.payload as Message),
          )

          // Compacting, and compacted. The summary is stored without an event
          // of its own, so the transcript is read again once it lands. The
          // snapshot carries its own cursor and already holds everything this
          // batch says, so the rest of the batch is skipped, not replayed.
          if (result.events.some((e) => e.kind === 'chat.compacting')) setCompacting(true)
          if (result.events.some((e) => e.kind === 'chat.compacted')) {
            setCompacting(false)
            setCompactions((n) => n + 1)
            cursor = await reload()
            continue
          }

          // Asleep, and awake again. The note it wakes to is the one thing both
          // ways of waking write, so it is what clears the banner -- in every
          // tab, including the one that did not press the button.
          const sleeping = result.events.find((e) => e.kind === 'chat.sleeping')
          if (sleeping) {
            setHeld({ message: 'Asleep', resumable: true, asleep: sleeping.payload as Asleep })
          }
          if (
            result.events.some(
              (e) => e.kind === 'chat.message' && (e.payload as Message).metadata?.wake,
            )
          ) {
            setHeldAt((prev) => (prev?.session === sessionId && prev.hold.asleep ? null : prev))
          }

          // A tool announces itself before the reply that used it, so it
          // Any event about a reply means its turn is still going. `job_state`
          // was only ever written when a turn *finished*, so the working
          // indicator animated only while the last fetch happened to catch the
          // job as pending or running -- and stopped at the first tool call,
          // because nothing afterwards said the turn was still alive. A reply
          // that streamed for two more minutes sat there looking finished.
          for (const event of result.events) {
            if (
              event.kind !== 'chat.delta' &&
              event.kind !== 'chat.reasoning' &&
              event.kind !== 'chat.tool' &&
              event.kind !== 'chat.tool_result'
            ) {
              continue
            }
            const { message_id } = event.payload as { message_id?: string }
            if (!message_id) continue
            setMessages((prev) => {
              const reply = prev.find((m) => m.id === message_id)
              // Only while the prompt it answers is not already finished: a
              // late event arriving after `chat.done` must not restart the
              // animation on a turn that has stopped.
              if (!reply?.replies_to) return prev
              const prompt = prev.find((m) => m.id === reply.replies_to)
              if (!prompt || prompt.job_state === 'succeeded') return prev
              if (prompt.job_state === 'running') return prev
              return prev.map((m) =>
                m.id === reply.replies_to ? { ...m, job_state: 'running' } : m,
              )
            })
          }

          // lands on the message already on screen rather than appearing
          // after the answer it explains.
          //
          // First, a call the model has begun writing: a card where the call
          // will be, before any of its arguments, which take seconds and are
          // not sent until whole. Without it that time looked like nothing
          // happening, and the thought before it kept counting through it.
          for (const event of result.events) {
            if (event.kind !== 'chat.writing') continue
            const { message_id, name } = event.payload
            // By the event, not by `index`: the index starts again every round,
            // and one round's card is not the next round's.
            const id = `${PREPARING}${event.id}`
            setMessages((prev) =>
              prev.map((m) => {
                if (m.id !== message_id) return m
                const calls = m.metadata.tool_calls ?? []
                if (calls.some((c) => c.id === id)) return m
                return {
                  ...m,
                  metadata: {
                    ...m.metadata,
                    tool_calls: [...calls, { id, name, action: 'Preparing…' }],
                    parts: [...(m.metadata.parts ?? []), { type: 'call', id }],
                  },
                }
              }),
            )
          }

          for (const event of result.events) {
            if (event.kind !== 'chat.tool') continue
            const { message_id, call } = event.payload as {
              message_id: string
              call: ToolCallRecord
            }
            callTools.current.set(call.id, call.name)
            setMessages((prev) =>
              prev.map((m) => {
                if (m.id !== message_id) return m
                const calls = m.metadata.tool_calls ?? []
                // The same call can arrive twice if a poll overlaps a
                // reload, and a tool run once must not be drawn twice.
                if (calls.some((c) => c.id === call.id)) return m
                const parts = [...(m.metadata.parts ?? [])]
                // Into the card that was being prepared for it, where one was:
                // calls start in the order they were written, so the first
                // card still preparing is this call's.
                const card = calls.find((c) => c.id.startsWith(PREPARING))
                if (card) {
                  return {
                    ...m,
                    metadata: {
                      ...m.metadata,
                      tool_calls: calls.map((c) => (c.id === card.id ? call : c)),
                      parts: parts.map((p) =>
                        p.type === 'call' && p.id === card.id ? { type: 'call', id: call.id } : p,
                      ),
                    },
                  }
                }
                parts.push({ type: 'call', id: call.id })
                return {
                  ...m,
                  metadata: { ...m.metadata, tool_calls: [...calls, call], parts },
                }
              }),
            )
          }

          // A tool's result lands on the call it answers, so the browser
          // holds one object per tool rather than two to reconcile.
          let moved = false
          for (const event of result.events) {
            if (event.kind !== 'chat.tool_result') continue
            const { message_id, id, details, is_error } = event.payload as {
              message_id: string
              id: string
              details: string
              is_error: boolean
            }
            // Refreshed on a failure too: an archive that failed halfway may
            // have written some of its files, and a list is cheap to read.
            if (FILE_TOOLS.has(callTools.current.get(id) ?? '')) moved = true
            callTools.current.delete(id)
            setMessages((prev) =>
              prev.map((m) => {
                if (m.id !== message_id) return m
                const calls = (m.metadata.tool_calls ?? []).map((c) =>
                  c.id === id ? { ...c, details, is_error } : c,
                )
                return { ...m, metadata: { ...m.metadata, tool_calls: calls } }
              }),
            )
          }

          if (moved) filesChanged.current?.()

          // A message taken mid-turn is answered inside the running reply.
          // Marking it is what stops it reading as queued for ever.
          for (const event of result.events) {
            if (event.kind !== 'chat.absorbed') continue
            const { message_id, absorbed_by } = event.payload as {
              message_id: string
              absorbed_by: string
            }
            setMessages((prev) =>
              prev.map((m) => (m.id === message_id ? { ...m, absorbed_by } : m)),
            )
          }

          // A retry reuses the reply it already made, so nothing in the
          // transcript changes; only the event says the turn started over.
          for (const event of result.events) {
            if (event.kind !== 'chat.retry') continue
            const { message_id } = event.payload as { message_id: string }
            setRetrying((prev) => new Set(prev).add(message_id))
          }

          // Deltas append to a message that already exists, so what is on
          // screen during generation is the same object that remains after,
          // and nothing is swapped when the turn completes.
          let gapped = false
          for (const event of result.events) {
            // In the same pass as the text, so a message taken mid-turn lands
            // between the words before it and the words answering it.
            if (event.kind === 'chat.steer') {
              const { message_id, id } = event.payload as { message_id: string; id: string }
              setMessages((prev) =>
                prev.map((m) => {
                  if (m.id !== message_id) return m
                  const parts = m.metadata.parts ?? []
                  if (parts.some((p) => p.type === 'steer' && p.id === id)) return m
                  return {
                    ...m,
                    metadata: { ...m.metadata, parts: [...parts, { type: 'steer', id }] },
                  }
                }),
              )
              continue
            }
            if (event.kind === 'chat.reasoning') {
              // No index to check. A delta is reconciled against stored content
              // -- a gap means the reply would be wrong -- but thinking is never
              // folded into content, so there is nothing for a missing fragment
              // to corrupt and a refetch would only show it twice.
              const { message_id, text } = event.payload
              // Timed from the events' own ids, which are when the API stored
              // each fragment -- the clock a reload measures with, so a thought
              // reads the same duration streamed as reloaded. Measured here
              // rather than waiting for the stored parts, which a tab that
              // watched the turn never reads: its thoughts showed no time at
              // all until somebody reloaded.
              const at = mintedAt(event.id)
              // When this tab heard it, for ticking the figure between
              // fragments. Its own clock only: the duration is the server's,
              // and this measures nothing but how long since that was right.
              const seenAt = Date.now()
              setMessages((prev) =>
                prev.map((m) => {
                  if (m.id !== message_id) return m
                  // Appended to the run of thinking in progress, or opened as a
                  // new one. The same rule the API applies, so what streams and
                  // what a reload rebuilds are the same message.
                  const parts = [...(m.metadata.parts ?? [])]
                  const last = parts[parts.length - 1]
                  if (last && last.type === 'reasoning') {
                    // A thought this tab did not see begin -- it was reloaded
                    // partway -- is taken to have run up to its stored length
                    // without a gap, which errs by at most one poll.
                    const since =
                      thinkingSince.current.get(m.id) ??
                      (at === null ? undefined : at - (last.ms ?? 0))
                    if (since !== undefined) thinkingSince.current.set(m.id, since)
                    const ms = at === null || since === undefined ? last.ms : Math.max(0, at - since)
                    parts[parts.length - 1] = {
                      type: 'reasoning',
                      text: last.text + text,
                      ...(ms === undefined ? {} : { ms, seenAt }),
                    }
                  } else {
                    if (at === null) thinkingSince.current.delete(m.id)
                    else thinkingSince.current.set(m.id, at)
                    // Zero, measured, where it could be timed: a thought just
                    // begun has taken no time yet, and ticks from here.
                    parts.push(
                      at === null ? { type: 'reasoning', text } : { type: 'reasoning', text, ms: 0, seenAt },
                    )
                  }
                  return { ...m, metadata: { ...m.metadata, parts } }
                }),
              )
              continue
            }
            if (event.kind !== 'chat.delta') continue
            const { message_id, idx, text } = event.payload as {
              message_id: string
              idx: number
              text: string
            }
            const expected = deltaProgress.current.get(message_id) ?? 0
            // Anything at or below the expected index is already folded into
            // the content; anything above it means a fragment went missing.
            if (idx !== expected) {
              if (idx > expected) gapped = true
              continue
            }
            deltaProgress.current.set(message_id, idx + 1)
            setMessages((prev) =>
              prev.map((m) => {
                if (m.id !== message_id) return m
                // Kept in step with the content, so a reply reads the same
                // while it streams as it does once stored. Without this the
                // order only appears on reload, and a preamble jumps below
                // the call it came before the moment that call arrives.
                const parts = [...(m.metadata.parts ?? [])]
                const last = parts[parts.length - 1]
                if (last && last.type === 'text') {
                  parts[parts.length - 1] = { type: 'text', text: last.text + text }
                } else {
                  parts.push({ type: 'text', text })
                }
                return {
                  ...m,
                  content: m.content + text,
                  metadata: { ...m.metadata, parts },
                }
              }),
            )
            // Text arriving is the end of any retry: the turn is underway.
            setRetrying((prev) => {
              if (!prev.has(message_id)) return prev
              const next = new Set(prev)
              next.delete(message_id)
              return next
            })
          }

          // A missing delta cannot be reconstructed from the stream, so the
          // snapshot is taken again rather than left with a hole in it.
          if (gapped) {
            cursor = await reload()
            if (stopped) return
          }

          for (const event of result.events) {
            if (event.kind === 'session.renamed') {
              renamed.current?.(event.payload.title)
              continue
            }
            if (event.kind !== 'chat.done') continue
            const { message_id } = event.payload as { message_id: string }
            // The job behind the prompt this reply answers is finished; the
            // snapshot would say so, so the live view should too.
            setMessages((prev) => {
              const reply = prev.find((m) => m.id === message_id)
              if (!reply?.replies_to) return prev
              return prev.map((m) => {
                if (m.id === reply.replies_to) return { ...m, job_state: 'succeeded' }
                // A card still preparing when the turn ends is a call that
                // never started -- refused, or cut off with the round -- and a
                // spinner left on it would say it was still coming.
                if (m.id === message_id) return withoutPreparing(m)
                return m
              })
            })
            setSending(false)
            setStopping(false)
          }

          // A hold, before the failure check: the turn did not run, and the
          // placeholder is left alone because nothing was discarded
          // server-side. The reader is told what is holding it, not that
          // something broke.
          const holding = result.events.find((e) => e.kind === 'chat.held')
          if (holding) {
            const { message, resumable, approval, asleep } = holding.payload as {
              message: string
              resumable: boolean
              approval?: PendingApproval
              asleep?: Asleep | null
            }
            setHeld({
              message,
              resumable,
              approval: approval ?? undefined,
              asleep: asleep ?? undefined,
            })
            setSending(false)
            setStopping(false)
          }

          // A failed turn put back on the queue, by this reader or another
          // one. Whoever pressed the button has already had the mark change
          // under their hand; this is what tells everybody else, who would
          // otherwise go on being shown a failure that is no longer true.
          const retried = result.events.find((e) => e.kind === 'chat.requeued')
          if (retried) {
            const { message_id } = retried.payload as { message_id?: string }
            if (message_id) {
              setFailures((prev) => {
                if (!prev.has(message_id)) return prev
                const next = new Map(prev)
                next.delete(message_id)
                return next
              })
              // The stored state says `failed` until the next load, and
              // `annotate` reads it as well as the map -- so clearing one
              // without the other leaves the button where it was.
              setMessages((prev) =>
                prev.map((m) =>
                  m.id === message_id && m.job_state === 'failed'
                    ? { ...m, job_state: 'pending' }
                    : m,
                ),
              )
              setError(null)
              setSending(true)
            }
          }

          const failed = result.events.find((e) => e.kind === 'chat.error')
          if (failed) {
            const { message, message_id } = failed.payload as {
              message: string
              message_id?: string
            }
            setError(message)
            setSending(false)
            setStopping(false)
            if (message_id) {
              setFailures((prev) => new Map(prev).set(message_id, message))
              // A failed turn's empty reply is discarded server-side, and the
              // reader should not be left looking at a bubble that no longer
              // exists. A reply holding thinking is not discarded there and
              // must not be dropped here: it is the only account of where the
              // turn's tokens went.
              setMessages((prev) =>
                prev.filter(
                  (m) =>
                    !(
                      m.role === 'assistant' &&
                      m.replies_to === message_id &&
                      !hasContent(m)
                    ),
                ),
              )
            }
          }
        } catch {
          if (stopped) return
          // Back off so a downed API does not spin the loop.
          await new Promise((resolve) => setTimeout(resolve, 2000))
        }
      }
    })()

    return () => {
      stopped = true
      controller.abort()
    }
  }, [sessionId, merge])

  const submit = useCallback(
    async (message: AppendMessage, delivery?: Delivery) => {
      // Taken now rather than read after the awaits below: by then the reader
      // may have opened another conversation, or another agent's new chat,
      // and what this send does next is about the page it was sent from.
      const start = sessionId ? undefined : starting.current
      const target: (() => Promise<string>) | null = sessionId
        ? () => Promise.resolve(sessionId)
        : start
          ? () => start.create()
          : null
      if (!target) return
      /** Whether the reader is still looking at what this send began in. */
      const stillHere = () =>
        showing.current === sessionId && starting.current?.agent === start?.agent
      const part = message.content[0]
      if (part?.type !== 'text') {
        throw new Error('only text messages are supported')
      }

      // Taken at send rather than as it is pasted, so an image removed before
      // the message goes is an image the model never hears about.
      const references = attachments.current?.() ?? ''
      // A passage quoted out of a reply, which the composer carries in its
      // metadata until send. Written as an ordinary markdown quote rather
      // than as a structure: the model already knows what `>` means, the
      // transcript reads back the way it was written, and nothing new has to
      // be honoured anywhere. A richer shape can replace this later without
      // the quote having gone missing in the meantime.
      const quote = (message.metadata?.custom as { quote?: { text: string } } | undefined)?.quote
      const quoted = quote?.text
        ? `${quote.text
            .trim()
            .split('\n')
            .map((line) => `> ${line}`)
            .join('\n')}\n\n`
        : ''
      const body = `${quoted}${part.text}`
      const text = references ? `${body}\n\n${references}`.trim() : body

      setError(null)
      setHeldAt(null)
      setSending(true)
      let made: string | null = null
      try {
        const id = await target()
        if (!sessionId) made = id
        // The POST returns the stored user message; the reply arrives later
        // over the event feed.
        const stored = await sendMessage(id, text, delivery)
        // Merged only into the conversation it belongs to. The reader who
        // moved on meanwhile finds it there, and the list already has it.
        // `sending` too is left alone: it belongs to the page on screen now,
        // which was reset when it was opened and may be sending itself.
        if (!stillHere()) return
        // The POST does not say, but a message it accepted has a job queued
        // for it by construction: the two are written together.
        merge([{ ...stored, job_state: 'pending' }])
      } catch (e) {
        if (stillHere()) {
          setSending(false)
          setError(e instanceof Error ? e.message : 'failed to send')
        }
        throw e
      } finally {
        // Opened even when the send failed: the conversation exists by then,
        // and leaving the reader on a blank "new" page would hide it from
        // them. Not when they have left that page: taking them back to it
        // would undo a choice they made while waiting.
        if (made && stillHere()) start?.opened(made)
      }
    },
    [sessionId, merge],
  )

  /**
   * Declaring this is what lets someone type while the agent is working.
   *
   * Without it the composer swallows Enter mid-turn -- and swallows it by
   * returning early, so the keypress falls through to the textarea and inserts
   * a newline instead. It looks intermittent, because it only happens while a
   * reply is still streaming.
   *
   * The lanes are always empty because the queue is not here. A message is
   * persisted the moment it is sent and comes back over the event feed like
   * any other; the agent picks it up at its next round boundary. Holding a
   * copy in the browser as well would mean two places disagreeing about
   * whether something was sent, and the browser's copy is the one that
   * vanishes when the tab closes.
   */
  const queue = useMemo(
    () => ({
      items: [],
      steerItems: [],
      // Reached when nothing is running: an ordinary message starting an
      // ordinary turn.
      enqueue: (message: AppendMessage) => {
        void submit(message).catch(() => {})
      },
      // Reached when a turn is in flight. The agent is told at its next round
      // boundary, which is what someone typing mid-reply means.
      steer: (message: AppendMessage) => {
        void submit(message, 'steer').catch(() => {})
      },
      // Nothing is ever pending on this side, so there is nothing to reorder,
      // rewrite or take back.
      move: () => {},
      edit: () => {},
      remove: () => {},
    }),
    [submit],
  )

  const annotated = useMemo(
    () => splitAtSteers(annotate(messages, retrying, failures)),
    [messages, retrying, failures],
  )

  /**
   * Asks the turn to stop.
   *
   * The composer only offers a stop button when this exists, so its absence is
   * what made a turn unstoppable rather than any missing button.
   *
   * Nothing is hidden here. The words already on screen stay, because the
   * server keeps them too -- the agent returns what it had written and that is
   * the reply. Hiding them locally would mean the transcript changed under the
   * reader the moment it reloaded, which is the one thing this app has always
   * been careful not to do.
   *
   * There is a lag, and it is honest: the turn stops at its next round
   * boundary, so a little more text can arrive after the press. Pretending
   * otherwise would mean discarding words the transcript is about to keep.
   */
  const cancel = useCallback(async () => {
    if (!sessionId) return
    setStopping(true)
    try {
      await cancelTurn(sessionId)
    } catch (e) {
      // The turn may well have finished on its own between the press and the
      // request, which is a race nobody can avoid and not worth an error.
      setStopping(false)
      setError(e instanceof Error ? e.message : String(e))
    }
  }, [sessionId])

  /**
   * Runs a failed turn again.
   *
   * The failure is cleared here rather than waited for over the feed. The
   * server answers, then emits `chat.requeued`, then this tab polls -- which
   * is a second or more of a button that says Try again beside a turn that is
   * already trying. Put back if the call fails, so a refusal does not leave
   * somebody watching a mark for work nobody is doing.
   */
  const retry = useCallback(
    async (messageId: string) => {
      if (!sessionId) return
      const previous = failures.get(messageId)
      setFailures((prev) => {
        if (!prev.has(messageId)) return prev
        const next = new Map(prev)
        next.delete(messageId)
        return next
      })
      setMessages((prev) =>
        prev.map((m) =>
          m.id === messageId && m.job_state === 'failed' ? { ...m, job_state: 'pending' } : m,
        ),
      )
      setError(null)
      setSending(true)
      try {
        await retryTurn(sessionId, messageId)
      } catch (e) {
        if (previous !== undefined) {
          setFailures((prev) => new Map(prev).set(messageId, previous))
        }
        setMessages((prev) =>
          prev.map((m) =>
            m.id === messageId && m.job_state === 'pending' ? { ...m, job_state: 'failed' } : m,
          ),
        )
        setSending(false)
        setError(e instanceof Error ? e.message : 'could not run that turn again')
      }
    },
    [sessionId, failures],
  )

  // What this tab did, or what the transcript says is happening -- whichever
  // is true. The local half alone left a viewer of somebody else's session
  // with no stop button and no way to steer; the derived half alone would
  // flicker off between sending and the poll that first reports the job.
  const isRunning = sending || turnIsRunning(messages)

  const runtime = useExternalStoreRuntime({
    messages: annotated,
    isRunning,
    convertMessage,
    // Never reached while `queue` is set -- the runtime routes every append
    // through the queue instead -- but required, and the same path anyway.
    onNew: submit,
    onCancel: cancel,
    queue,
  })

  return useMemo(
    () => ({
      runtime,
      error,
      held,
      isRunning,
      stopping,
      compacting,
      compactions,
      retry,
      clearHeld,
      dismissError,
    }),
    [
      runtime,
      error,
      held,
      isRunning,
      stopping,
      compacting,
      compactions,
      retry,
      clearHeld,
      dismissError,
    ],
  )
}
