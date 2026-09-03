import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  useExternalStoreRuntime,
  type AppendMessage,
  type ThreadMessageLike,
} from '@assistant-ui/react'

import { loadMessages, pollEvents, sendMessage, type Message } from './chat'

/**
 * Our stored message, mapped to what assistant-ui renders.
 *
 * The id is carried through deliberately: it is what lets a message that
 * arrives over the event feed replace the one already on screen rather than
 * appear beside it. When streaming lands, deltas will append to the content of
 * this same id, so there is never a separate "streaming" object to swap in.
 */
const convertMessage = (message: Message): ThreadMessageLike => ({
  id: message.id,
  role: message.role === 'tool' ? 'assistant' : message.role,
  content: [{ type: 'text', text: message.content }],
})

/** Highest contiguous delta index applied per message, for gap detection. */
type DeltaProgress = Map<string, number>

export function useChatRuntime(sessionId: string | null) {
  const [messages, setMessages] = useState<Message[]>([])
  const [isRunning, setIsRunning] = useState(false)
  const [error, setError] = useState<string | null>(null)

  // Read inside the poll loop without making it a dependency, so switching
  // session does not tear down and rebuild the loop mid-request.
  const seen = useRef<Set<string>>(new Set())
  const deltaProgress = useRef<DeltaProgress>(new Map())

  /**
   * Appends a fragment to a message already on screen.
   *
   * Deltas carry an index so a gap is detectable: applying one out of order
   * would silently corrupt the text, where noticing lets the message be
   * refetched instead.
   */
  const applyDelta = useCallback(
    (messageId: string, idx: number, text: string): boolean => {
      const expected = deltaProgress.current.get(messageId) ?? 0
      if (idx !== expected) {
        return false
      }
      deltaProgress.current.set(messageId, idx + 1)
      setMessages((prev) =>
        prev.map((m) => (m.id === messageId ? { ...m, content: m.content + text } : m)),
      )
      return true
    },
    [],
  )

  const merge = useCallback((incoming: Message[]) => {
    if (incoming.length === 0) return
    setMessages((prev) => {
      const known = new Set(prev.map((m) => m.id))
      const added = incoming.filter((m) => !known.has(m.id))
      if (added.length === 0) return prev
      return [...prev, ...added].sort((a, b) => a.seq - b.seq)
    })
  }, [])

  // Load history when the session changes.
  useEffect(() => {
    seen.current = new Set()
    let cancelled = false
    if (!sessionId) {
      // Deferred rather than synchronous, so this does not cascade a render.
      queueMicrotask(() => {
        if (!cancelled) setMessages([])
      })
      return () => {
        cancelled = true
      }
    }
    void loadMessages(sessionId)
      .then((loaded) => {
        if (cancelled) return
        setMessages(loaded)
        setIsRunning(false)
      })
      .catch(() => {
        if (!cancelled) setMessages([])
      })
    return () => {
      cancelled = true
    }
  }, [sessionId])

  // Long poll for this session.
  useEffect(() => {
    if (!sessionId) return
    const controller = new AbortController()
    let cursor = 0
    let stopped = false

    void (async () => {
      while (!stopped) {
        try {
          const result = await pollEvents(sessionId, cursor, controller.signal)
          if (stopped) return
          cursor = result.cursor

          const arrived = result.events
            .filter((e) => e.kind === 'chat.message')
            .map((e) => e.payload as Message)
          merge(arrived)

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
            if (!applyDelta(message_id, idx, text)) {
              gapped = true
            }
          }

          // A missing delta cannot be reconstructed from the stream, so the
          // message is reloaded rather than left with a hole in it.
          if (gapped && sessionId) {
            const fresh = await loadMessages(sessionId)
            deltaProgress.current = new Map()
            setMessages(fresh)
          }

          const done = result.events.filter((e) => e.kind === 'chat.done')
          if (done.length > 0) {
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
  }, [sessionId, merge, applyDelta])

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
