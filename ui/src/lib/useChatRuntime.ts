import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  useExternalStoreRuntime,
  type AppendMessage,
  type ThreadMessageLike,
} from '@assistant-ui/react'

import {
  loadHistory,
  pollEvents,
  sendMessage,
  type Delivery,
  type Message,
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
  | { kind: 'failed'; message: string }

type Annotated = Message & { status?: MessageStatus | null }

/**
 * Works out each user message's status from the transcript around it.
 *
 * `retrying` is the one thing the transcript cannot tell: a retry reuses the
 * same empty reply, so it is remembered from the event until a delta arrives.
 */
function annotate(
  messages: Message[],
  retrying: Set<string>,
  failures: Map<string, string>,
): Annotated[] {
  const replyFor = new Map<string, Message>()
  for (const m of messages) {
    if (m.role === 'assistant' && m.replies_to) replyFor.set(m.replies_to, m)
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

  return messages.map((m) => {
    if (m.role !== 'user') return m

    if (m.absorbed_by) {
      return { ...m, status: inProgress.has(m.absorbed_by) ? { kind: 'absorbed' } : null }
    }

    const reply = replyFor.get(m.id)
    const replyUnderway =
      reply !== undefined &&
      (reply.content !== '' || (reply.metadata.tool_calls?.length ?? 0) > 0)
    if (replyUnderway) return { ...m, status: null }

    const failure = failures.get(m.id)
    if (failure !== undefined || m.job_state === 'failed') {
      return { ...m, status: { kind: 'failed', message: failure ?? 'the turn failed' } }
    }

    if (reply) {
      return { ...m, status: { kind: retrying.has(reply.id) ? 'retrying' : 'waiting' } }
    }

    switch (m.job_state) {
      case 'pending':
        return { ...m, status: { kind: replyInProgress ? 'steering' : 'queued' } }
      case 'running':
        return { ...m, status: { kind: 'waiting' } }
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
  metadata: { custom: { status: message.status ?? null } },
  content: [
    // Tools lead the reply, because that is the order they happened in: the
    // agent went and looked something up, then answered.
    ...(message.metadata.tool_calls ?? []).map((call) => ({
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
      },
      argsText: JSON.stringify({ action: call.action }),
    })),
    { type: 'text' as const, text: message.content },
  ] as ThreadMessageLike['content'],
})

/** Ids are UUIDv7, so lexical order is insertion order. */
const byId = (a: Message, b: Message) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)

/** Where the next delta belongs, per message id. */
type DeltaProgress = Map<string, number>

export function useChatRuntime(sessionId: string | null) {
  const [messages, setMessages] = useState<Message[]>([])
  const [isRunning, setIsRunning] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const deltaProgress = useRef<DeltaProgress>(new Map())
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
      return history.cursor
    }

    void (async () => {
      let cursor: string
      try {
        cursor = await reload()
        if (stopped) return
        setIsRunning(false)
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

          // A tool announces itself before the reply that used it, so it
          // lands on the message already on screen rather than appearing
          // after the answer it explains.
          for (const event of result.events) {
            if (event.kind !== 'chat.tool') continue
            const { message_id, call } = event.payload as {
              message_id: string
              call: ToolCallRecord
            }
            setMessages((prev) =>
              prev.map((m) => {
                if (m.id !== message_id) return m
                const calls = m.metadata.tool_calls ?? []
                // The same call can arrive twice if a poll overlaps a
                // reload, and a tool run once must not be drawn twice.
                if (calls.some((c) => c.id === call.id)) return m
                return { ...m, metadata: { ...m.metadata, tool_calls: [...calls, call] } }
              }),
            )
          }

          // A tool's result lands on the call it answers, so the browser
          // holds one object per tool rather than two to reconcile.
          for (const event of result.events) {
            if (event.kind !== 'chat.tool_result') continue
            const { message_id, id, details, is_error } = event.payload as {
              message_id: string
              id: string
              details: string
              is_error: boolean
            }
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
              prev.map((m) =>
                m.id === message_id ? { ...m, content: m.content + text } : m,
              ),
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
            if (event.kind !== 'chat.done') continue
            const { message_id } = event.payload as { message_id: string }
            // The job behind the prompt this reply answers is finished; the
            // snapshot would say so, so the live view should too.
            setMessages((prev) => {
              const reply = prev.find((m) => m.id === message_id)
              if (!reply?.replies_to) return prev
              return prev.map((m) =>
                m.id === reply.replies_to ? { ...m, job_state: 'succeeded' } : m,
              )
            })
            setIsRunning(false)
          }

          const failed = result.events.find((e) => e.kind === 'chat.error')
          if (failed) {
            const { message, message_id } = failed.payload as {
              message: string
              message_id?: string
            }
            setError(message)
            setIsRunning(false)
            if (message_id) {
              setFailures((prev) => new Map(prev).set(message_id, message))
              // A failed turn's empty reply is discarded server-side, and the
              // reader should not be left looking at a bubble that no longer
              // exists.
              setMessages((prev) =>
                prev.filter(
                  (m) => !(m.role === 'assistant' && m.replies_to === message_id && m.content === ''),
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
      if (!sessionId) return
      const part = message.content[0]
      if (part?.type !== 'text') {
        throw new Error('only text messages are supported')
      }

      setError(null)
      setIsRunning(true)
      try {
        // The POST returns the stored user message; the reply arrives later
        // over the event feed.
        const stored = await sendMessage(sessionId, part.text, delivery)
        // The POST does not say, but a message it accepted has a job queued
        // for it by construction: the two are written together.
        merge([{ ...stored, job_state: 'pending' }])
      } catch (e) {
        setIsRunning(false)
        setError(e instanceof Error ? e.message : 'failed to send')
        throw e
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
    () => annotate(messages, retrying, failures),
    [messages, retrying, failures],
  )

  const runtime = useExternalStoreRuntime({
    messages: annotated,
    isRunning,
    convertMessage,
    // Never reached while `queue` is set -- the runtime routes every append
    // through the queue instead -- but required, and the same path anyway.
    onNew: submit,
    queue,
  })

  return useMemo(() => ({ runtime, error, isRunning }), [runtime, error, isRunning])
}
