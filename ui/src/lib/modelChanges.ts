import { createContext, useContext } from 'react'

import { api } from './api'

/** A reply answered by a model other than the one its chain tried first. */
export type ModelChange = {
  reply_id: string
  configured_model: string
  configured_name: string | null
  served_model: string
  served_name: string | null
  /** Why the first was passed over, as a clause: "was rate limited". */
  reason: string
}

export const listModelChanges = (sessionId: string) =>
  api<ModelChange[]>(`/v1/agent-sessions/${sessionId}/model-changes`)

export type ModelChangesState = {
  byReply: Map<string, ModelChange>
  /** Says a reply has finished, so the changes are read again if it is one
   *  not yet seen: a change is only recorded as a reply is written. */
  finished: (replyId: string) => void
}

export const ModelChangesContext = createContext<ModelChangesState>({
  byReply: new Map(),
  finished: () => {},
})

export const useModelChanges = () => useContext(ModelChangesContext)
