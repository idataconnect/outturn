import { renderHook, waitFor } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import { useChatRuntime } from './useChatRuntime'
import type { Message } from './chat'

const msg = (m: Partial<Message> & Pick<Message, 'id' | 'role'>): Message => ({
  content: '',
  metadata: {},
  delta_next: 0,
  model: null,
  replies_to: null,
  absorbed_by: null,
  job_state: null,
  ...m,
})

let polled = false
vi.mock('./chat', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./chat')>()
  return {
    ...actual,
    loadHistory: vi.fn(async () => ({
      messages: [
        msg({ id: '01a0-u1', role: 'user', content: 'what day?', job_state: 'running' }),
        msg({ id: '01a0-a1', role: 'assistant', replies_to: '01a0-u1' }),
        msg({ id: '01a0-u2', role: 'user', content: 'what else?', job_state: 'pending' }),
      ],
      cursor: 'c0',
      has_more: false,
    })),
    pollEvents: vi.fn(async () => {
      if (polled) return new Promise(() => {})
      polled = true
      return {
        cursor: 'c1',
        events: [
          { id: 'e1', kind: 'chat.absorbed', payload: { message_id: '01a0-u2', absorbed_by: '01a0-a1' } },
          { id: 'e2', kind: 'chat.tool', payload: { message_id: '01a0-a1', call: { id: 'c1', name: 'load_tools', action: 'Loading' } } },
          { id: 'e3', kind: 'chat.steer', payload: { message_id: '01a0-a1', id: '01a0-u2' } },
          { id: 'e4', kind: 'chat.delta', payload: { message_id: '01a0-a1', idx: 0, text: 'I can do lots.' } },
        ],
      }
    }),
  }
})

describe('a message taken mid-turn, as the feed reports it', () => {
  it('is drawn inside the reply before the turn finishes', async () => {
    const { result } = renderHook(() => useChatRuntime('s1'))
    const ids = () =>
      (result.current.runtime as unknown as {
        thread: { getState: () => { messages: { id: string }[] } }
      }).thread
        .getState()
        .messages.map((m) => m.id)

    await waitFor(() => expect(ids()).toEqual(['01a0-u1', '01a0-a1', '01a0-u2', '01a0-a1:1']))
  })
})
