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
  roles: string[]
  authorities: string[]
}

export type SessionState =
  | { status: 'loading' }
  | { status: 'anonymous' }
  | {
      status: 'authenticated'
      session: Session
      displayName: string
      tenants: TenantMembership[]
    }

export type SessionActions = {
  signIn: (displayName: string, tenants: TenantMembership[]) => void
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
