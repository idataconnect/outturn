import { renderHook, act } from '@testing-library/react'
import { describe, expect, it, vi, beforeEach } from 'vitest'

import { useChatRuntime } from './useChatRuntime'

const sent: string[] = []

vi.mock('./chat', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./chat')>()
  return {
    ...actual,
    sendMessage: vi.fn(async (_id: string, content: string) => {
      sent.push(content)
      return {
        id: 'u1',
        role: 'user' as const,
        content,
        metadata: {},
        delta_next: 0,
        model: null,
      }
    }),
    readMessages: vi.fn(async () => ({ messages: [], cursor: '0', has_more: false })),
    pollEvents: vi.fn(() => new Promise(() => {})),
  }
})

/** Sends one message through the runtime, as the composer would. */
async function send(take: (() => string) | null, text: string) {
  const { result } = renderHook(() =>
    useChatRuntime('s1', undefined, take ?? undefined),
  )
  await act(async () => {
    await (result.current.runtime as unknown as {
      thread: { append: (m: unknown) => Promise<void> }
    }).thread.append({ role: 'user', content: [{ type: 'text', text }] })
  })
}

describe('sending a message with an image attached', () => {
  beforeEach(() => {
    sent.length = 0
  })

  it('carries the image reference in the message', async () => {
    // The model is told a path because that is what `describe_image` takes:
    // put it in the message and the agent can look without being asked twice.
    await send(() => '[image: session/a.png]', 'what is this?')
    expect(sent[0]).toBe('what is this?\n\n[image: session/a.png]')
  })

  it('sends the text alone when nothing is attached', async () => {
    await send(() => '', 'just talking')
    expect(sent[0]).toBe('just talking')
  })

  it('does not need a composer that attaches anything', async () => {
    await send(null, 'plain')
    expect(sent[0]).toBe('plain')
  })
})
