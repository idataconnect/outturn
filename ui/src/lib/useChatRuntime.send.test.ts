import { act, renderHook } from '@testing-library/react'
import type { AppendMessage } from '@assistant-ui/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { sendMessage, type Message } from './chat'
import { useChatRuntime } from './useChatRuntime'

// What the hook handed the runtime last, so a test can send through `onNew`
// the way the composer does, and read back what the thread would draw.
type Config = { onNew: (m: AppendMessage) => Promise<void>; messages: Array<{ id: string }> }
let latest: Config
vi.mock('@assistant-ui/react', () => ({
  useExternalStoreRuntime: (config: Config) => {
    latest = config
    return {}
  },
}))

vi.mock('./chat', () => ({
  sendMessage: vi.fn(),
  loadHistory: vi.fn(async () => ({ messages: [], cursor: '' })),
  // A feed that never says anything, so nothing but the send touches the page.
  pollEvents: vi.fn(() => new Promise(() => {})),
  cancelTurn: vi.fn(),
  retryTurn: vi.fn(),
}))

/** A composer's message, with only what `submit` reads. The runtime's own
 *  type carries a good deal more that sending never looks at. */
const said = (text: string) =>
  ({ content: [{ type: 'text', text }], metadata: {} }) as unknown as AppendMessage

const stored = (id: string): Message => ({
  id,
  role: 'user',
  content: 'hello',
  metadata: {},
  delta_next: 0,
  model: null,
  replies_to: null,
  absorbed_by: null,
  job_state: null,
})

const draft = (create: () => Promise<string>) => ({ agent: 'a1', create: vi.fn(create), opened: vi.fn() })

/** A promise and the means to settle it from the test. */
function later<T>() {
  let resolve!: (v: T) => void
  const promise = new Promise<T>((r) => (resolve = r))
  return { promise, resolve }
}

describe('a new chat, made by its first message', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('is created, sent into, and opened', async () => {
    vi.mocked(sendMessage).mockResolvedValue(stored('m1'))
    const fresh = draft(async () => 's-new')
    renderHook(() => useChatRuntime(null, undefined, undefined, fresh))

    await act(() => latest.onNew(said('hello')))

    expect(fresh.create).toHaveBeenCalledTimes(1)
    expect(sendMessage).toHaveBeenCalledWith('s-new', 'hello', undefined)
    expect(fresh.opened).toHaveBeenCalledWith('s-new')
  })

  it('is opened even when the message then fails to send', async () => {
    // The conversation exists by the time the send fails; staying on an
    // empty "new" page would hide it from the person who made it.
    vi.mocked(sendMessage).mockRejectedValue(new Error('no'))
    const fresh = draft(async () => 's-new')
    renderHook(() => useChatRuntime(null, undefined, undefined, fresh))

    await act(() => expect(latest.onNew(said('hello'))).rejects.toThrow('no'))

    expect(fresh.opened).toHaveBeenCalledWith('s-new')
  })

  it('is still opened when the page re-renders with a new hook for the same agent', async () => {
    // What says the reader is still here is who the chat is with, not which
    // object the parent happened to pass on its latest render.
    const made = later<string>()
    vi.mocked(sendMessage).mockResolvedValue(stored('m1'))
    const first = draft(() => made.promise)
    const again = { ...first }
    const { rerender } = renderHook(
      ({ f }: { f: typeof first }) => useChatRuntime(null, undefined, undefined, f),
      { initialProps: { f: first } },
    )

    let sent!: Promise<void>
    act(() => {
      sent = latest.onNew(said('hello'))
    })
    rerender({ f: again })
    await act(async () => {
      made.resolve('s-new')
      await sent
    })

    expect(first.opened).toHaveBeenCalledWith('s-new')
  })

  it('leaves alone a reader who moved on while it was being made', async () => {
    // Clicking into another conversation while the first message is on its
    // way used to put that message into the one clicked, and yank the reader
    // back afterwards to the chat they had just walked away from.
    const made = later<string>()
    vi.mocked(sendMessage).mockResolvedValue(stored('m1'))
    const fresh = draft(() => made.promise)
    const { rerender } = renderHook(
      ({ id, f }: { id: string | null; f?: typeof fresh }) =>
        useChatRuntime(id, undefined, undefined, f),
      { initialProps: { id: null as string | null, f: fresh as typeof fresh | undefined } },
    )

    let sent!: Promise<void>
    act(() => {
      sent = latest.onNew(said('hello'))
    })
    rerender({ id: 'other', f: undefined })
    await act(async () => {
      made.resolve('s-new')
      await sent
    })

    // Sent where it was meant for, and nowhere else.
    expect(sendMessage).toHaveBeenCalledWith('s-new', 'hello', undefined)
    expect(fresh.opened).not.toHaveBeenCalled()
    expect(latest.messages.some((m) => m.id === 'm1')).toBe(false)
  })
})
