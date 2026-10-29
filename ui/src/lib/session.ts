import { createContext, use } from 'react'

import { ApiError } from './api'

export type WorkspaceMembership = {
  workspace_id: string
  name: string
  slug: string
  roles: string[]
}

export type Session = {
  session_id: string
  workspace_id: string
  display_name: string
  roles: string[]
  authorities: string[]
  workspaces: WorkspaceMembership[]
  /** Whether the message box offers a microphone in this workspace. Absent
   *  from an API older than the setting, which is read as allowed. */
  dictation?: boolean
}

export type SessionState =
  /**
   * Nothing is known yet. `reconnecting` means the API did not answer and is
   * being retried -- a down backend says nothing about whether there is a
   * session, so it must not be mistaken for being signed out.
   */
  | { status: 'loading'; reconnecting?: boolean }
  | { status: 'anonymous' }
  | {
      status: 'authenticated'
      session: Session
      displayName: string
      workspaces: WorkspaceMembership[]
    }

export type SessionActions = {
  /** Called once the credentials are accepted; the session is read back from
   *  the API rather than passed in, so every entry point agrees on it. */
  signIn: () => void
  signOut: () => void
  switchWorkspace: (workspaceId: string) => Promise<void>
}

export const SessionContext = createContext<SessionState>({ status: 'loading' })
export const SessionActionsContext = createContext<SessionActions | null>(null)

export function useSession(): SessionState {
  return use(SessionContext)
}

export function useSessionActions(): SessionActions {
  const actions = use(SessionActionsContext)
  if (!actions) throw new Error('useSessionActions used outside SessionProvider')
  return actions
}

/** Authority checks are advisory here — the API enforces them regardless. */
export function useHasAuthority(authority: string): boolean {
  const state = useSession()
  return state.status === 'authenticated' && state.session.authorities.includes(authority)
}

/** How long to wait before asking the API again: one second, then two,
 *  four, and every ten once it has been a while, so a long outage is not
 *  asked about many times a second by every open tab. */
export function retryDelay(attempt: number): number {
  return Math.min(1000 * 2 ** attempt, 10_000)
}

/**
 * The session the API holds, or null where it says there is none.
 *
 * Only the API saying "not you" -- a 401, which `api` returns only once a
 * refresh has been refused too -- means signed out, and it is believed at
 * once. A backend that is down, restarting or erroring says nothing about
 * the session, so that is asked again for as long as it takes: giving up used
 * to show the sign-in form, which could not sign anybody in while the API
 * was down, over a session that was still good once it came back.
 */
export async function restoreSession<T>(
  read: () => Promise<T>,
  onReconnecting: () => void,
  wait: (ms: number) => Promise<void> = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
): Promise<T | null> {
  for (let attempt = 0; ; attempt++) {
    try {
      return await read()
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) return null
      onReconnecting()
      await wait(retryDelay(attempt))
    }
  }
}
