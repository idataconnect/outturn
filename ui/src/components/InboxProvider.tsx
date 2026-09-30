import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'

import { countActionItems, listActionItems, type ActionItem } from '../lib/actions'
import { InboxContext } from '../lib/inbox'

/** The least time between two reads, however quickly the first came back. */
const MIN_INTERVAL_MS = 1_000

/**
 * Follows the reader's queue for as long as they are signed in.
 *
 * The server parks each request until something concerning them lands --
 * raised, answered by a colleague, woken -- and answers with the queue as it
 * stands, so an item somebody else settled leaves this list without anybody
 * reloading. The count is read beside it, because the list is one page and the
 * badge must not stop at fifty.
 *
 * A change landing in the moment between one poll returning and the next being
 * sent waits for the next timeout, twenty-five seconds at most: the queue is a
 * set with no cursor to catch up from. Settling something here refreshes at
 * once, which covers the case that matters -- the reader's own answer.
 */
export default function InboxProvider({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<ActionItem[]>([])
  const [count, setCount] = useState({ count: 0, capped: false })
  const [loaded, setLoaded] = useState(false)
  const nudge = useRef<AbortController | null>(null)
  /** Set by `refresh`, so a read wanted at once is not parked instead -- the
   *  abort alone misses when nothing is in flight to cut short. */
  const wanted = useRef(true)

  useEffect(() => {
    let stopped = false

    async function read(wait: boolean) {
      const controller = new AbortController()
      nudge.current = controller
      const page = await listActionItems({ wait, signal: controller.signal })
      const counted = await countActionItems()
      if (stopped) return
      setItems(page.items ?? [])
      setCount(counted)
      setLoaded(true)
    }

    void (async () => {
      while (!stopped) {
        const started = Date.now()
        const now = wanted.current
        wanted.current = false
        try {
          await read(!now)
          // A parked request should take seconds. One that came back at once --
          // the API shutting down, a proxy answering for it -- is not asked
          // again straight away, or this becomes a loop as fast as the network.
          const spent = Date.now() - started
          if (spent < MIN_INTERVAL_MS) {
            await new Promise((r) => setTimeout(r, MIN_INTERVAL_MS - spent))
          }
        } catch (e) {
          if (stopped) return
          // Cut short on purpose by `refresh`, which has asked for a read at
          // once: go round again.
          if (e instanceof DOMException && e.name === 'AbortError') continue
          // Anything else -- the API restarting, a dropped connection -- is
          // waited out rather than retried in a tight loop.
          await new Promise((r) => setTimeout(r, 5_000))
        }
      }
    })()

    return () => {
      stopped = true
      nudge.current?.abort()
    }
  }, [])

  // Asks for a read at once, abandoning the parked request if there is one; the
  // loop reads, then parks again.
  const refresh = useCallback(() => {
    wanted.current = true
    nudge.current?.abort()
  }, [])

  const value = useMemo(
    () => ({ items, count: count.count, capped: count.capped, loaded, refresh }),
    [items, count, loaded, refresh],
  )
  return <InboxContext.Provider value={value}>{children}</InboxContext.Provider>
}
