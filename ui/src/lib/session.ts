import { createContext, use } from 'react'

export type TenantMembership = {
  tenant_id: string
  name: string
  slug: string
  roles: string[]
}

export type Session = {
  session_id: string
  tenant_id: string
  display_name: string
  roles: string[]
  authorities: string[]
  tenants: TenantMembership[]
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
      tenants: TenantMembership[]
    }

export type SessionActions = {
  /** Called once the credentials are accepted; the session is read back from
   *  the API rather than passed in, so every entry point agrees on it. */
  signIn: () => void
  signOut: () => void
  switchTenant: (tenantId: string) => Promise<void>
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
