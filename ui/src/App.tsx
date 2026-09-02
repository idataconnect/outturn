import { useCallback, useEffect, useMemo, useState } from 'react'
import { BrowserRouter, NavLink, Navigate, Route, Routes } from 'react-router'
import {
  Bot,
  Building2,
  LayoutDashboard,
  LogOut,
  MessageSquare,
  Settings,
  Users as UsersIcon,
} from 'lucide-react'

import TenantSwitcher from './components/TenantSwitcher'
import { api } from './lib/api'
import {
  SessionActionsContext,
  SessionContext,
  useSession,
  useSessionActions,
  type Session,
  type SessionActions,
  type SessionState,
  type TenantMembership,
} from './lib/session'
import Login from './pages/Login'
import Agents from './pages/Agents'
import Chat from './pages/Chat'
import Tenants from './pages/Tenants'
import Users from './pages/Users'

function Dashboard() {
  const state = useSession()
  return (
    <div className="p-6">
      <h1 className="text-2xl font-semibold text-gray-900 dark:text-gray-100">Dashboard</h1>
      <p className="mt-2 text-gray-600 dark:text-gray-400">
        {state.status === 'authenticated'
          ? `Signed in as ${state.displayName}.`
          : 'Overview coming soon.'}
      </p>
    </div>
  )
}

function SettingsPage() {
  return (
    <div className="p-6">
      <h1 className="text-2xl font-semibold text-gray-900 dark:text-gray-100">Settings</h1>
      <p className="mt-2 text-gray-600 dark:text-gray-400">Configuration coming soon.</p>
    </div>
  )
}

// `authority` gates visibility; the API enforces the same rule on every call.
const navItems = [
  { to: '/', icon: LayoutDashboard, label: 'Dashboard' },
  { to: '/agents', icon: Bot, label: 'Agents', authority: 'agents:read' },
  { to: '/sessions', icon: MessageSquare, label: 'Sessions' },
  { to: '/users', icon: UsersIcon, label: 'Users', authority: 'users:read' },
  { to: '/tenants', icon: Building2, label: 'Tenants', authority: 'tenants:read' },
  { to: '/settings', icon: Settings, label: 'Settings' },
]

type LoginResult = {
  display_name: string
  tenants: TenantMembership[]
}

function useSessionState(): [SessionState, SessionActions] {
  const [state, setState] = useState<SessionState>({ status: 'loading' })
  const [displayName, setDisplayName] = useState('')
  const [tenants, setTenants] = useState<TenantMembership[]>([])

  // The session cookie is HttpOnly, so its presence cannot be checked here:
  // the API is the only thing that can say whether there is a session.
  useEffect(() => {
    let cancelled = false
    // api() refreshes and retries on a 401, so a session whose access token
    // expired while the tab was closed is restored rather than dropped.
    api<Session>('/v1/session')
      .then((session) => {
        if (cancelled) return
        setState({ status: 'authenticated', session, displayName: '', tenants: [] })
      })
      .catch(() => {
        if (cancelled) return
        setState({ status: 'anonymous' })
      })
    return () => {
      cancelled = true
    }
  }, [])

  const signIn = useCallback(
    (name: string, memberships: TenantMembership[]) => {
      setDisplayName(name)
      setTenants(memberships)
      api<Session>('/v1/session')
        .then((session) =>
          setState({
            status: 'authenticated',
            session,
            displayName: name,
            tenants: memberships,
          }),
        )
        .catch(() => {
          setState({ status: 'anonymous' })
        })
    },
    [],
  )

  const signOut = useCallback(() => {
    // The cookie is HttpOnly, so only the server can clear it.
    void api<void>('/v1/logout', { method: 'POST' }).finally(() => {
      setDisplayName('')
      setTenants([])
      setState({ status: 'anonymous' })
    })
  }, [])

  const switchTenant = useCallback(
    async (tenantId: string) => {
      // The response re-sets the session cookie for the new tenant.
      const result = await api<LoginResult>('/v1/session/tenant', {
        method: 'POST',
        body: JSON.stringify({ tenant_id: tenantId }),
      })
      const session = await api<Session>('/v1/session')
      setState({
        status: 'authenticated',
        session,
        displayName: result.display_name,
        tenants: result.tenants,
      })
    },
    [],
  )

  // Keep name and tenant list across a session refetch that lacks them.
  const merged: SessionState = useMemo(() => {
    if (state.status !== 'authenticated') return state
    return {
      ...state,
      displayName: state.displayName || displayName,
      tenants: state.tenants.length ? state.tenants : tenants,
    }
  }, [state, displayName, tenants])

  return [merged, { signIn, signOut, switchTenant }]
}

function RequireAuthority({
  authority,
  children,
}: {
  authority: string
  children: React.ReactNode
}) {
  const state = useSession()
  if (state.status === 'loading') return null
  if (state.status !== 'authenticated' || !state.session.authorities.includes(authority)) {
    return <Navigate to="/" replace />
  }
  return <>{children}</>
}

function Shell() {
  const state = useSession()
  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const visible = navItems.filter((item) => !item.authority || authorities.includes(item.authority))

  return (
    <div className="flex h-screen bg-gray-50 dark:bg-gray-950">
      <nav className="w-56 border-r border-gray-200 dark:border-gray-800 bg-white dark:bg-gray-900 flex flex-col">
        <div className="p-4 border-b border-gray-200 dark:border-gray-800">
          <h1 className="text-lg font-semibold text-gray-900 dark:text-gray-100">outturn</h1>
        </div>
        <div className="p-2 border-b border-gray-200 dark:border-gray-800">
          <TenantSwitcher />
        </div>
        <div className="flex-1 p-2 space-y-1">
          {visible.map(({ to, icon: Icon, label }) => (
            <NavLink
              key={to}
              to={to}
              end={to === '/'}
              className={({ isActive }) =>
                `flex items-center gap-2 px-3 py-2 rounded-md text-sm transition-colors ${
                  isActive
                    ? 'bg-gray-100 dark:bg-gray-800 text-gray-900 dark:text-gray-100'
                    : 'text-gray-600 dark:text-gray-400 hover:bg-gray-50 dark:hover:bg-gray-800/50'
                }`
              }
            >
              <Icon size={16} />
              {label}
            </NavLink>
          ))}
        </div>
        <SignOut />
      </nav>
      <main className="flex-1 overflow-auto">
        <Routes>
          <Route path="/" element={<Dashboard />} />
          <Route path="/sessions" element={<Chat />} />
          <Route
            path="/agents"
            element={
              <RequireAuthority authority="agents:read">
                <Agents />
              </RequireAuthority>
            }
          />
          <Route path="/settings" element={<SettingsPage />} />
          <Route
            path="/users"
            element={
              <RequireAuthority authority="users:read">
                <Users />
              </RequireAuthority>
            }
          />
          <Route
            path="/tenants"
            element={
              <RequireAuthority authority="tenants:read">
                <Tenants />
              </RequireAuthority>
            }
          />
        </Routes>
      </main>
    </div>
  )
}

function SignOut() {
  const state = useSession()
  const { signOut } = useSessionActions()
  if (state.status !== 'authenticated') return null

  return (
    <div className="p-2 border-t border-gray-200 dark:border-gray-800">
      <p className="px-3 pt-1 pb-2 text-xs text-gray-500 dark:text-gray-400 truncate">
        {state.session.roles.join(', ')}
      </p>
      <button
        onClick={signOut}
        className="w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm text-gray-600 dark:text-gray-400 hover:bg-gray-50 dark:hover:bg-gray-800/50"
      >
        <LogOut size={16} />
        Sign out
      </button>
    </div>
  )
}

function App() {
  const [state, actions] = useSessionState()

  return (
    <SessionContext value={state}>
      <SessionActionsContext value={actions}>
        {state.status === 'loading' ? null : state.status === 'anonymous' ? (
          <Login />
        ) : (
          <BrowserRouter>
            <Shell />
          </BrowserRouter>
        )}
      </SessionActionsContext>
    </SessionContext>
  )
}

export default App
