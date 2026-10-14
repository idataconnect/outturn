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

const reply = '01a0-a1'
let polls = 0
vi.mock('./chat', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./chat')>()
  return {
    ...actual,
    loadHistory: vi.fn(async () => ({
      messages: [
        msg({ id: '01a0-u1', role: 'user', content: 'Is it free?', job_state: 'running' }),
        msg({ id: reply, role: 'assistant', replies_to: '01a0-u1' }),
      ],
      cursor: 'c0',
      has_more: false,
    })),
    // Two batches, cut where a fast model's stream was cut: the first carries
    // the start of a sentence, the second its end, the call after it, the
    // call's result and the answer.
    pollEvents: vi.fn(async () => {
      polls += 1
      if (polls === 1) {
        return {
          cursor: 'c1',
          events: [
            { id: 'e1', kind: 'chat.delta', payload: { message_id: reply, idx: 0, text: 'Saturday is 2026' } },
          ],
        }
      }
      if (polls === 2) {
        return {
          cursor: 'c2',
          events: [
            { id: 'e2', kind: 'chat.delta', payload: { message_id: reply, idx: 1, text: '-10-17.' } },
            { id: 'e3', kind: 'chat.writing', payload: { message_id: reply, index: 0, name: 'fetch_url', bytes: 0 } },
            { id: 'e4', kind: 'chat.tool', payload: { message_id: reply, call: { id: 'c1', name: 'fetch_url', action: 'Checking' } } },
            { id: 'e5', kind: 'chat.tool_result', payload: { message_id: reply, id: 'c1', details: '{}', is_error: false } },
            { id: 'e6', kind: 'chat.delta', payload: { message_id: reply, idx: 2, text: 'Two rooms are free.' } },
          ],
        }
      }
      return new Promise(() => {})
    }),
  }
})

describe('a reply streamed in batches', () => {
  it('keeps a call after the sentence that came before it', async () => {
    const { result } = renderHook(() => useChatRuntime('s1'))
    const shape = () => {
      const messages = (result.current.runtime as unknown as {
        thread: {
          getState: () => {
            messages: {
              id: string
              content: { type: string; text?: string; toolCallId?: string; args?: { pending?: boolean } }[]
            }[]
          }
        }
      }).thread.getState().messages
      return messages
        .filter((m) => m.id.startsWith(reply))
        .flatMap((m) => m.content)
        .map((p) => (p.type === 'text' ? `text:${p.text}` : `${p.type}:${p.toolCallId}:${p.args?.pending ? 'pending' : 'done'}`))
    }

    // The sentence whole, then the call -- with the result that shared its
    // batch -- then the answer.
    await waitFor(() =>
      expect(shape()).toEqual([
        'text:Saturday is 2026-10-17.',
        'tool-call:c1:done',
        'text:Two rooms are free.',
      ]),
    )
  })
})
