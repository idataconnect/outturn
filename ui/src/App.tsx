import { useCallback, useEffect, useState } from 'react'
import { BrowserRouter, NavLink, Navigate, Route, Routes, useLocation } from 'react-router'
import {
  Bot,
  Building2,
  LayoutDashboard,
  MessageSquare,
  Settings,
  BookText,
  Users as UsersIcon,
  KeyRound,
  Menu,
  PanelLeftClose,
  X,
} from 'lucide-react'

import AccountMenu from './components/AccountMenu'
import SettingsCascade from './components/SettingsCascade'
import { ApiError, api } from './lib/api'
import { readFlag, storeFlag } from './lib/layout'
import { useBreakpoint } from './lib/useBreakpoint'
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
import Skills from './pages/Skills'
import SkillEditor from './pages/SkillEditor'
import AgentEditor from './pages/AgentEditor'
import Chat from './pages/Chat'
import Workspaces from './pages/Workspaces'
import Roles from './pages/Roles'
import RoleEditor from './pages/RoleEditor'
import WorkspaceEditor from './pages/WorkspaceEditor'
import Users from './pages/Users'
import UserEditor from './pages/UserEditor'
import { iconButton, iconButtonLarge } from './lib/buttons'

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
  { to: '/skills', icon: BookText, label: 'Skills', authority: 'skills:read' },
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

  const breakpoint = useBreakpoint()
  const phone = breakpoint === 'phone'
  // Labelled by default where there is room, icons-only once someone asks for
  // the space back. On a phone neither: it is a drawer, and opening it shows
  // the labels because a drawer has the width to spare.
  const [expanded, setExpanded] = useState(() => readFlag('nav.expanded', true))
  const [drawer, setDrawer] = useState(false)

  function toggle() {
    if (phone) {
      setDrawer((v) => !v)
      return
    }
    setExpanded((v: boolean) => {
      storeFlag('nav.expanded', !v)
      return !v
    })
  }

  // A drawer covers the page, so leaving the page should take it with you.
  const location = useLocation()
  useEffect(() => setDrawer(false), [location.pathname])

  // Labels are shown in the drawer even though it is a phone: the drawer is
  // wide, and an icon rail the reader deliberately opened should say what its
  // icons mean.
  const labelled = phone ? true : expanded
  const railed = !phone && !expanded

  return (
    <div className="flex h-screen bg-surface-100 dark:bg-surface-900">
      {phone && drawer && (
        <div
          className="fixed inset-0 z-30 bg-black/30"
          onClick={() => setDrawer(false)}
          aria-hidden
        />
      )}

      <nav
        className={`${phone ? (drawer ? 'fixed inset-y-0 left-0 z-40 w-56 shadow-xl flex' : 'hidden') : 'flex'} ${
          railed ? 'w-14' : 'w-56'
        } shrink-0 border-r border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 flex-col transition-[width]`}
      >
        <div
          className={`flex items-center gap-2 border-b border-surface-200 dark:border-surface-800 ${
            railed ? 'justify-center p-3' : 'p-4'
          }`}
        >
          {/* The mark is the toggle, both ways. Railed it is the only way
              back out, and it would be a strange control that could open
              this column but not shut it again -- so it does both, and the
              chevron beside it is a second route to the same thing rather
              than the only one.

              Not on a phone: there the nav is a drawer, the mark would
              mean "close", and the X already says that more plainly. */}
          {phone ? (
            <img src="/favicon.svg" alt="" className="w-6 h-6 shrink-0" />
          ) : (
            <button
              type="button"
              onClick={toggle}
              aria-label={expanded ? 'outturn -- collapse navigation' : 'outturn -- expand navigation'}
              aria-expanded={expanded}
              title={expanded ? 'Collapse navigation' : 'Expand navigation'}
              className={iconButton}
            >
              <img src="/favicon.svg" alt="" className="w-6 h-6 shrink-0" />
            </button>
          )}
          {labelled && (
            <h1 className="flex-1 text-lg font-semibold text-surface-900 dark:text-surface-100">
              outturn
            </h1>
          )}
          {!phone && expanded && (
            <button
              type="button"
              onClick={toggle}
              aria-label="Collapse navigation"
              aria-expanded
              title="Collapse navigation"
              className={iconButton}
            >
              <PanelLeftClose size={16} aria-hidden />
            </button>
          )}
          {phone && (
            <button
              type="button"
              onClick={() => setDrawer(false)}
              aria-label="Close navigation"
              className={iconButton}
            >
              <X size={16} aria-hidden />
            </button>
          )}
        </div>
        <div className="flex-1 p-2 space-y-1">
          {visible.map(({ to, icon: Icon, label }) => (
            <NavLink
              key={to}
              to={to}
              end={to === '/'}
              // The name has to survive the label going away, or a rail of
              // unexplained glyphs is all that is left.
              title={label}
              aria-label={label}
              className={({ isActive }) =>
                `flex items-center gap-2 rounded-md text-sm transition-colors ${
                  railed ? 'justify-center px-0 py-2' : 'px-3 py-2'
                } ${
                  isActive
                    ? 'bg-brand-50 dark:bg-brand-950 text-brand-800 dark:text-brand-200 font-medium'
                    : 'text-surface-600 dark:text-surface-400 hover:bg-surface-50 dark:hover:bg-surface-800/50'
                }`
              }
            >
              <Icon size={16} className="shrink-0" />
              {labelled && label}
            </NavLink>
          ))}
        </div>
        <div className="p-2 border-t border-surface-200 dark:border-surface-800">
          <AccountMenu collapsed={railed} />
        </div>
      </nav>
      <div className="flex-1 flex flex-col min-w-0">
        {phone && (
          <header className="flex items-center gap-2 px-2 py-2 border-b border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900">
            <button
              type="button"
              onClick={toggle}
              aria-label="Open navigation"
              aria-expanded={drawer}
              className={iconButtonLarge}
            >
              <Menu size={18} aria-hidden />
            </button>
            <img src="/favicon.svg" alt="" className="w-5 h-5 shrink-0" />
            <span className="text-sm font-semibold text-surface-900 dark:text-surface-100">
              outturn
            </span>
          </header>
        )}
        <main className="flex-1 min-h-0 overflow-auto bg-surface-50 dark:bg-surface-875">
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
            path="/skills"
            element={
              <RequireAuthority authority="skills:read">
                <Skills />
              </RequireAuthority>
            }
          />
          <Route
            path="/skills/new"
            element={
              <RequireAuthority authority="skills:write">
                <SkillEditor />
              </RequireAuthority>
            }
          />
          {/* Read opens it; the page decides whether it can be written. */}
          <Route
            path="/skills/:id"
            element={
              <RequireAuthority authority="skills:read">
                <SkillEditor />
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
