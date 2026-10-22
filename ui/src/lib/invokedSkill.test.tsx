import { renderHook, act } from '@testing-library/react'
import { describe, expect, it, vi, beforeEach } from 'vitest'

import { useChatRuntime } from './useChatRuntime'

const sent: unknown[][] = []

vi.mock('./chat', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./chat')>()
  return {
    ...actual,
    sendMessage: vi.fn(async (...args: unknown[]) => {
      sent.push(args)
      return {
        id: 'u1',
        role: 'user' as const,
        content: args[1],
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
async function send(message: unknown) {
  const { result } = renderHook(() => useChatRuntime('s1'))
  await act(async () => {
    await (result.current.runtime as unknown as {
      thread: { append: (m: unknown) => Promise<void> }
    }).thread.append(message)
  })
}

const skill = { custom: { skill: { slug: 'raise-invoice', name: 'Raise an Invoice' } } }

describe('sending a message that invoked a skill', () => {
  beforeEach(() => {
    sent.length = 0
  })

  it('sends the slug beside the words, not in them', async () => {
    await send({ role: 'user', content: [{ type: 'text', text: 'for PO 1182' }], runConfig: skill })
    expect(sent).toHaveLength(1)
    const [, content, , slug] = sent[0]
    expect(content).toBe('for PO 1182')
    expect(slug).toBe('raise-invoice')
  })

  it('sends a pick with nothing typed', async () => {
    await send({ role: 'user', content: [], runConfig: skill })
    expect(sent).toHaveLength(1)
    const [, content, , slug] = sent[0]
    expect(content).toBe('')
    expect(slug).toBe('raise-invoice')
  })

  it('sends no skill when none was picked', async () => {
    await send({ role: 'user', content: [{ type: 'text', text: 'plain' }] })
    expect(sent[0]).toHaveLength(3)
  })
})
