import { createContext, useContext } from 'react'

import { api } from './api'
import type { Actor } from './actors'

/** A person's verdict on a reply, or on the whole conversation. */
export type Verdict = 'up' | 'down'

export type Feedback = {
  id: string
  /** The reply it is about; null for the conversation as a whole. */
  message_id: string | null
  verdict: Verdict
  note: string
  by: Actor
  /** The reader's own, which is the one they may change. */
  mine: boolean
  updated_at: string
}

export const listFeedback = (sessionId: string) =>
  api<Feedback[]>(`/v1/agent-sessions/${sessionId}/feedback`)

export const giveFeedback = (
  sessionId: string,
  input: { message_id: string | null; verdict: Verdict; note: string },
) =>
  api<void>(`/v1/agent-sessions/${sessionId}/feedback`, {
    method: 'PUT',
    body: JSON.stringify(input),
  })

export const withdrawFeedback = (sessionId: string, messageId: string | null) =>
  api<void>(
    `/v1/agent-sessions/${sessionId}/feedback${messageId ? `?message_id=${messageId}` : ''}`,
    { method: 'DELETE' },
  )

/** Which reply's panel is open, and with which thumb. */
export type Opened = { messageId: string; verdict: Verdict } | null

export type FeedbackState = {
  sessionId: string | null
  all: Feedback[]
  opened: Opened
  open: (opened: Opened) => void
  /** Re-read after a change, so everyone's verdicts stay as the server has them. */
  reload: () => Promise<void>
}

export const FeedbackContext = createContext<FeedbackState>({
  sessionId: null,
  all: [],
  opened: null,
  open: () => {},
  reload: async () => {},
})

export const useFeedback = () => useContext(FeedbackContext)
