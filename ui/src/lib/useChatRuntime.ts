import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  useExternalStoreRuntime,
  type AppendMessage,
  type ThreadMessageLike,
} from '@assistant-ui/react'

import { loadHistory, pollEvents, sendMessage, type Message } from './chat'

/**
 * Our stored message, mapped to what assistant-ui renders.
 *
 * The id is carried through deliberately: it is what lets a message that
 * arrives over the event feed replace the one already on screen rather than
 * appear beside it. Deltas append to the content of this same id, so there is
 * never a separate "streaming" object to swap in.
 */
const convertMessage = (message: Message): ThreadMessageLike => ({
  id: message.id,
  role: message.role === 'tool' ? 'assistant' : message.role,
  content: [{ type: 'text', text: message.content }],
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
      queueMicrotask(() => setMessages([]))
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
          }

          // A missing delta cannot be reconstructed from the stream, so the
          // snapshot is taken again rather than left with a hole in it.
          if (gapped) {
            cursor = await reload()
            if (stopped) return
          }

          if (result.events.some((e) => e.kind === 'chat.done')) {
            setIsRunning(false)
          }

          const failed = result.events.find((e) => e.kind === 'chat.error')
          if (failed) {
            setError((failed.payload as { message: string }).message)
            setIsRunning(false)
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

  const onNew = useCallback(
    async (message: AppendMessage) => {
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
        const stored = await sendMessage(sessionId, part.text)
        merge([stored])
      } catch (e) {
        setIsRunning(false)
        setError(e instanceof Error ? e.message : 'failed to send')
        throw e
      }
    },
    [sessionId, merge],
  )

  const runtime = useExternalStoreRuntime({
    messages,
    isRunning,
    convertMessage,
    onNew,
  })

  return useMemo(() => ({ runtime, error, isRunning }), [runtime, error, isRunning])
}
