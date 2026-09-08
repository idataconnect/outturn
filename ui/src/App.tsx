import { useCallback, useEffect, useState } from 'react'
import { BrowserRouter, NavLink, Navigate, Route, Routes } from 'react-router'
import {
  Bot,
  Building2,
  LayoutDashboard,
  MessageSquare,
  Settings,
  Users as UsersIcon,
  KeyRound,
} from 'lucide-react'

import AccountMenu from './components/AccountMenu'
import SettingsCascade from './components/SettingsCascade'
import { ApiError, api } from './lib/api'
import {
  SessionActionsContext,
  SessionContext,
  useSession,
  type Session,
  type SessionActions,
  type SessionState,
} from './lib/session'
import Login from './pages/Login'
import Agents from './pages/Agents'
import AgentEditor from './pages/AgentEditor'
import Chat from './pages/Chat'
import Workspaces from './pages/Workspaces'
import Roles from './pages/Roles'
import RoleEditor from './pages/RoleEditor'
import WorkspaceEditor from './pages/WorkspaceEditor'
import Users from './pages/Users'
import UserEditor from './pages/UserEditor'

function Dashboard() {
  const state = useSession()
  return (
    <div className="p-6">
      <h1 className="text-2xl font-semibold text-surface-900 dark:text-surface-100">Dashboard</h1>
      <p className="mt-2 text-surface-600 dark:text-surface-400">
        {state.status === 'authenticated'
          ? `Signed in as ${state.displayName}.`
          : 'Overview coming soon.'}
      </p>
    </div>
  )
}

function SettingsPage() {
  const state = useSession()
  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canEdit = authorities.includes('settings:update')
  const isOperator =
    state.status === 'authenticated' && state.session.roles.includes('system_admin')
  const [tab, setTab] = useState<'workspace' | 'platform'>('workspace')

  return (
    <div className="p-6 max-w-3xl space-y-6">
      <div>
        <h1 className="text-2xl font-semibold text-surface-900 dark:text-surface-100">Settings</h1>
        <p className="mt-2 text-surface-600 dark:text-surface-400">
          How agents in this workspace behave. Each value comes from the platform unless
          overridden here, and an agent can override again on its own page.
        </p>
      </div>

      {isOperator && (
        <div className="flex gap-1 border-b border-surface-200 dark:border-surface-800">
          {(
            [
              { key: 'workspace', label: 'This workspace' },
              { key: 'platform', label: 'Platform defaults' },
            ] as const
          ).map((t) => (
            <button
              key={t.key}
              type="button"
              onClick={() => setTab(t.key)}
              className={`px-3 py-2 text-sm font-medium border-b-2 -mb-px ${
                tab === t.key
                  ? 'border-brand-600 text-surface-900 dark:text-surface-100'
                  : 'border-transparent text-surface-500 dark:text-surface-400 hover:text-surface-800 dark:hover:text-surface-200'
              }`}
            >
              {t.label}
            </button>
          ))}
        </div>
      )}

      {(!isOperator || tab === 'workspace') && (
        <SettingsCascade base="/v1/settings" canEdit={canEdit} levelName="this workspace" />
      )}

      {isOperator && tab === 'platform' && (
        <section className="space-y-4">
          <p className="text-sm text-surface-600 dark:text-surface-400">
            What every workspace gets unless it overrides. Visible to the operator only.
          </p>
          <SettingsCascade base="/v1/platform/settings" canEdit levelName="the platform" />
        </section>
      )}
    </div>
  )
}

// `authority` gates visibility; the API enforces the same rule on every call.
const navItems = [
  { to: '/', icon: LayoutDashboard, label: 'Dashboard' },
  { to: '/agents', icon: Bot, label: 'Agents', authority: 'agents:read' },
  { to: '/sessions', icon: MessageSquare, label: 'Sessions' },
  { to: '/users', icon: UsersIcon, label: 'Users', authority: 'users:read' },
  { to: '/roles', icon: KeyRound, label: 'Roles', authority: 'roles:assign' },
  { to: '/workspaces', icon: Building2, label: 'Workspaces', authority: 'workspaces:read' },
  { to: '/settings', icon: Settings, label: 'Settings' },
]

function useSessionState(): [SessionState, SessionActions] {
  const [state, setState] = useState<SessionState>({ status: 'loading' })

  /**
   * The one place a session becomes state.
   *
   * Every entry point -- first load, signing in, switching workspace -- reads the
   * session back from the API rather than assembling it from whatever the
   * calling response happened to contain. A cold load has only this endpoint,
   * so anything it cannot supply is missing on every reload.
   */
  const load = useCallback(async () => {
    // Only the API saying "not you" means signed out. A backend that is down,
    // restarting or erroring says nothing about the session, and treating that
    // as a logout throws away a session the server still holds -- during a
    // redeploy every reload dropped the user at the login form while their
    // refresh token stayed perfectly valid. So a 401 is decisive and anything
    // else is retried.
    for (let attempt = 0; ; attempt++) {
      try {
        // api() refreshes and retries on a 401, so a session whose access
        // token expired while the tab was closed is restored rather than
        // dropped.
        const session = await api<Session>('/v1/session')
        setState({
          status: 'authenticated',
          session,
          displayName: session.display_name,
          workspaces: session.workspaces,
        })
        return
      } catch (e) {
        if (e instanceof ApiError && e.status === 401) {
          setState({ status: 'anonymous' })
          return
        }
        // Say so rather than sitting on a blank page: the retries take
        // several seconds, and silence during them reads as a broken app.
        setState((prev) =>
          prev.status === 'loading' ? { status: 'loading', reconnecting: true } : prev,
        )

        if (attempt >= 4) {
          // Out of patience. An unreachable API is indistinguishable from no
          // session as far as this screen can tell.
          setState({ status: 'anonymous' })
          return
        }
        await new Promise((resolve) => setTimeout(resolve, 1000 * (attempt + 1)))
      }
    }
  }, [])

  // The session cookie is HttpOnly, so its presence cannot be checked here:
  // the API is the only thing that can say whether there is a session.
  useEffect(() => {
    void load()
  }, [load])

  const signIn = useCallback(() => {
    void load()
  }, [load])

  const signOut = useCallback(() => {
    // The cookie is HttpOnly, so only the server can clear it.
    void api<void>('/v1/logout', { method: 'POST' }).finally(() => {
      setState({ status: 'anonymous' })
    })
  }, [])

  const switchWorkspace = useCallback(
    async (workspaceId: string) => {
      // The response re-sets the session cookie for the new workspace.
      await api<unknown>('/v1/session/workspace', {
        method: 'POST',
        body: JSON.stringify({ workspace_id: workspaceId }),
      })
      await load()
    },
    [load],
  )

  return [state, { signIn, signOut, switchWorkspace }]
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
    <div className="flex h-screen bg-surface-100 dark:bg-surface-950">
      <nav className="w-56 border-r border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 flex flex-col">
        <div className="flex items-center gap-2 p-4 border-b border-surface-200 dark:border-surface-800">
          <img src="/favicon.svg" alt="" className="w-6 h-6 shrink-0" />
          <h1 className="text-lg font-semibold text-surface-900 dark:text-surface-100">outturn</h1>
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
                    ? 'bg-brand-50 dark:bg-brand-950 text-brand-800 dark:text-brand-200 font-medium'
                    : 'text-surface-600 dark:text-surface-400 hover:bg-surface-50 dark:hover:bg-surface-800/50'
                }`
              }
            >
              <Icon size={16} />
              {label}
            </NavLink>
          ))}
        </div>
        <div className="p-2 border-t border-surface-200 dark:border-surface-800">
          <AccountMenu />
        </div>
      </nav>
      <main className="flex-1 overflow-auto bg-surface-50 dark:bg-surface-900">
        {/* Keyed by workspace so a switch remounts every page. State loaded
            under the previous workspace -- lists, editors, an open thread --
            is gone rather than shown until something happens to refetch it. */}
        <Routes key={state.status === 'authenticated' ? state.session.workspace_id : 'anon'}>
          <Route path="/" element={<Dashboard />} />
          <Route path="/sessions" element={<Chat />} />
          <Route path="/sessions/:sessionId" element={<Chat />} />
          <Route
            path="/agents"
            element={
              <RequireAuthority authority="agents:read">
                <Agents />
              </RequireAuthority>
            }
          />
          <Route
            path="/agents/new"
            element={
              <RequireAuthority authority="agents:create">
                <AgentEditor />
              </RequireAuthority>
            }
          />
          {/* Read opens it; the form itself decides whether it can be saved. */}
          <Route
            path="/agents/:id"
            element={
              <RequireAuthority authority="agents:read">
                <AgentEditor />
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
            path="/users/new"
            element={
              <RequireAuthority authority="users:create">
                <UserEditor />
              </RequireAuthority>
            }
          />
          <Route
            path="/users/:id"
            element={
              <RequireAuthority authority="users:read">
                <UserEditor />
              </RequireAuthority>
            }
          />
          <Route
            path="/roles"
            element={
              <RequireAuthority authority="roles:assign">
                <Roles />
              </RequireAuthority>
            }
          />
          <Route
            path="/roles/new"
            element={
              <RequireAuthority authority="roles:manage">
                <RoleEditor />
              </RequireAuthority>
            }
          />
          <Route
            path="/roles/:id"
            element={
              <RequireAuthority authority="roles:assign">
                <RoleEditor />
              </RequireAuthority>
            }
          />
          <Route
            path="/workspaces"
            element={
              <RequireAuthority authority="workspaces:read">
                <Workspaces />
              </RequireAuthority>
            }
          />
          <Route
            path="/workspaces/new"
            element={
              <RequireAuthority authority="workspaces:create">
                <WorkspaceEditor />
              </RequireAuthority>
            }
          />
          <Route
            path="/workspaces/:id"
            element={
              <RequireAuthority authority="workspaces:update">
                <WorkspaceEditor />
              </RequireAuthority>
            }
          />
        </Routes>
      </main>
    </div>
  )
}


/**
 * Shown while the session is being established.
 *
 * Deliberately not nothing: restoring a session can take a few seconds when
 * the API is slow to answer, and an empty document is indistinguishable from
 * a crash.
 */
function Loading({ reconnecting }: { reconnecting?: boolean }) {
  return (
    <div className="flex h-dvh flex-col items-center justify-center gap-3">
      {/* The same mark the shell uses, so the app looks like itself while
          it is still deciding what to show. */}
      <img src="/favicon.svg" alt="" className="h-8 w-8 animate-pulse" />
      {reconnecting && (
        <p className="text-sm text-surface-600 dark:text-surface-400">
          Reconnecting&hellip;
        </p>
      )}
    </div>
  )
}

function App() {
  const [state, actions] = useSessionState()

  return (
    <SessionContext value={state}>
      <SessionActionsContext value={actions}>
        {state.status === 'loading' ? (
          <Loading reconnecting={state.reconnecting} />
        ) : state.status === 'anonymous' ? (
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
