import { useCallback, useEffect, useState } from 'react'
import { BrowserRouter, NavLink, Navigate, Route, Routes, useLocation } from 'react-router'
import {
  Bot,
  Building2,
  Globe,
  LayoutDashboard,
  MessageSquare,
  Settings,
  SlidersHorizontal,
  BookText,
  Users as UsersIcon,
  KeyRound,
  Menu,
  PanelLeftClose,
  PanelLeftOpen,
  X,
} from 'lucide-react'

import AccountMenu from './components/AccountMenu'
import Logo from './components/Logo'
import { productName } from './lib/brand'
import SectionLayout from './components/SectionLayout'
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
import Dashboard from './pages/Dashboard'
import Workspaces from './pages/Workspaces'
import Roles from './pages/Roles'
import RoleEditor from './pages/RoleEditor'
import WorkspaceEditor from './pages/WorkspaceEditor'
import Users from './pages/Users'
import UserEditor from './pages/UserEditor'
import WorkspaceSettings from './pages/WorkspaceSettings'
import PlatformDefaults from './pages/PlatformDefaults'
import { paths } from './lib/paths'
import { iconButton, iconButtonLarge } from './lib/buttons'

// `authority` gates visibility; the API enforces the same rule on every call.
//
// Every item that leads somewhere authority-gated declares it. Dashboard and
// Settings did not, and showed to everyone: an operator clicking either was
// told "reading usage needs the usage:read authority" by a page they had been
// invited to open. A link that cannot work is worse than no link, because it
// reads as something broken rather than as something not theirs.
//
// `anyOf` is for an entry leading to a section: shown to anybody who may open
// any page in it. `divider` starts the part of the list reached rarely --
// administration, after the work.
//
// `under` nests an item beneath another: sessions are conversations *with*
// agents, so they sit under Agents rather than beside Skills. Only when the
// parent is shown -- somebody who may not see agents still has their sessions,
// and an indent under nothing reads as a mistake.
const navItems: {
  to: string
  icon: typeof Bot
  label: string
  authority?: string
  anyOf?: string[]
  under?: string
  divider?: boolean
}[] = [
  { to: '/', icon: LayoutDashboard, label: 'Dashboard', authority: 'usage:read' },
  { to: '/agents', icon: Bot, label: 'Agents', authority: 'agents:read' },
  { to: '/sessions', icon: MessageSquare, label: 'Sessions', under: '/agents' },
  { to: '/skills', icon: BookText, label: 'Skills', authority: 'skills:read' },
  {
    to: paths.settings,
    icon: Settings,
    label: 'Settings',
    anyOf: ['settings:read', 'users:read', 'roles:assign'],
    divider: true,
  },
  // Only a system administrator holds `workspaces:read`: no workspace may grant
  // it, which is what makes it the operator's.
  { to: paths.platform, icon: Globe, label: 'Platform', authority: 'workspaces:read' },
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

/** The same page at its new address, keeping whatever followed the old one. */
function Moved({ from, to }: { from: string; to: string }) {
  const { pathname, search } = useLocation()
  return <Navigate to={`${to}${pathname.slice(from.length)}${search}`} replace />
}

function useAuthorities(): string[] {
  const state = useSession()
  return state.status === 'authenticated' ? state.session.authorities : []
}

/** The workspace's own administration. */
function SettingsSection() {
  const authorities = useAuthorities()
  return (
    <SectionLayout
      title="Settings"
      items={[
        {
          to: paths.settings,
          label: 'Workspace',
          icon: SlidersHorizontal,
          allowed: authorities.includes('settings:read'),
          end: true,
        },
        { to: paths.users, label: 'Users', icon: UsersIcon, allowed: authorities.includes('users:read') },
        { to: paths.roles, label: 'Roles', icon: KeyRound, allowed: authorities.includes('roles:assign') },
      ]}
    />
  )
}

/**
 * Settings' landing page: the workspace's own settings, for anybody who may
 * read them, and otherwise the first page in the section they may open -- the
 * section is offered to somebody who can only manage users, and landing them
 * on a refusal would say the link was broken.
 */
function SettingsHome() {
  const authorities = useAuthorities()
  if (authorities.includes('settings:read')) return <WorkspaceSettings />
  if (authorities.includes('users:read')) return <Navigate to={paths.users} replace />
  if (authorities.includes('roles:assign')) return <Navigate to={paths.roles} replace />
  return <Navigate to="/" replace />
}

/** What crosses every workspace: the operator's alone. */
function PlatformSection() {
  const state = useSession()
  const operator = state.status === 'authenticated' && state.session.roles.includes('system_admin')
  return (
    <SectionLayout
      title="Platform"
      items={[
        { to: paths.workspaces, label: 'Workspaces', icon: Building2, allowed: true },
        { to: paths.platformDefaults, label: 'Defaults', icon: SlidersHorizontal, allowed: operator },
      ]}
    />
  )
}

function Shell() {
  const state = useSession()
  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const visible = navItems.filter(
    (item) =>
      (!item.authority || authorities.includes(item.authority)) &&
      (!item.anyOf || item.anyOf.some((a) => authorities.includes(a))),
  )

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
          // Top-aligned, with the same padding above in both states, so the
          // mark stays put when the nav opens: centred, it dropped to the
          // middle of a product name long enough to wrap.
          className={`flex items-start gap-2 border-b border-surface-200 dark:border-surface-800 ${
            railed ? 'justify-center p-3' : 'px-4 py-3'
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
            <Logo />
          ) : (
            <button
              type="button"
              onClick={toggle}
              aria-label={
                expanded
                  ? `${productName} -- collapse navigation`
                  : `${productName} -- expand navigation`
              }
              aria-expanded={expanded}
              title={expanded ? 'Collapse navigation' : 'Expand navigation'}
              // Not shrunk to make room for a long product name: it squeezed
              // the mark to 20px wide beside one that wrapped.
              className={`group shrink-0 ${iconButton}`}
            >
              {/* Railed, the mark gives way to the expand icon under the
                  pointer or the keyboard: nothing else on a rail says a logo
                  is a control, and this is the one way back out. Expanded it
                  stays the mark, since the chevron beside it already says
                  what clicking does. */}
              {railed ? (
                <>
                  <span className="flex group-hover:hidden group-focus-visible:hidden">
                    <Logo />
                  </span>
                  {/* In the mark's own box, so the rail does not shift. */}
                  <span className="hidden h-6 w-6 items-center justify-center group-hover:flex group-focus-visible:flex">
                    <PanelLeftOpen size={16} aria-hidden />
                  </span>
                </>
              ) : (
                <Logo />
              )}
            </button>
          )}
          {labelled && (
            // The first line level with the mark's 32px button.
            <h1 className="mt-0.5 flex-1 text-lg font-display font-semibold text-surface-900 dark:text-surface-100">
              {productName}
            </h1>
          )}
          {!phone && expanded && (
            <button
              type="button"
              onClick={toggle}
              aria-label="Collapse navigation"
              aria-expanded
              title="Collapse navigation"
              className={`shrink-0 ${iconButton}`}
            >
              {/* In a box the mark's size, so the two buttons are the same
                  height and line up whatever the rows around them do. */}
              <span className="flex h-6 w-6 items-center justify-center">
                <PanelLeftClose size={16} aria-hidden />
              </span>
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
          {visible.map(({ to, icon: Icon, label, under, divider }) => {
            const nested = Boolean(under) && visible.some((item) => item.to === under)
            return (
            <div key={to}>
            {divider && (
              <hr className="my-2 border-surface-200 dark:border-surface-800" aria-hidden />
            )}
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
                  railed ? 'justify-center px-0 py-2' : nested ? 'pl-8 pr-3 py-2' : 'px-3 py-2'
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
            </div>
            )
          })}
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
            <Logo className="w-5 h-5 shrink-0" />
            <span className="text-sm font-display font-semibold text-surface-900 dark:text-surface-100">
              {productName}
            </span>
          </header>
        )}
        <main className="flex-1 min-h-0 overflow-auto bg-surface-50 dark:bg-surface-875">
        {/* Keyed by workspace so a switch remounts every page. State loaded
            under the previous workspace -- lists, editors, an open thread --
            is gone rather than shown until something happens to refetch it. */}
        <Routes key={state.status === 'authenticated' ? state.session.workspace_id : 'anon'}>
          {/* Not gated by RequireAuthority: the page checks `usage:read`
              itself and says which authority is missing, which tells somebody
              who arrived by a bookmark more than a silent redirect would. The
              nav simply stops offering it. */}
          <Route path="/" element={<Dashboard />} />
          <Route path="/sessions" element={<Chat />} />
          <Route path="/sessions/new" element={<Chat draft />} />
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
          <Route
            path="/agents/:id"
            element={
              <RequireAuthority authority="agents:read">
                <Agents />
              </RequireAuthority>
            }
          />
          {/* Read opens it; the form itself decides whether it can be saved. */}
          <Route
            path="/agents/:id/edit"
            element={
              <RequireAuthority authority="agents:read">
                <AgentEditor />
              </RequireAuthority>
            }
          />
          <Route path={paths.settings} element={<SettingsSection />}>
            <Route index element={<SettingsHome />} />
            <Route
              path="users"
              element={
                <RequireAuthority authority="users:read">
                  <Users />
                </RequireAuthority>
              }
            />
            <Route
              path="users/new"
              element={
                <RequireAuthority authority="users:create">
                  <UserEditor />
                </RequireAuthority>
              }
            />
            <Route
              path="users/:id"
              element={
                <RequireAuthority authority="users:read">
                  <UserEditor />
                </RequireAuthority>
              }
            />
            <Route
              path="roles"
              element={
                <RequireAuthority authority="roles:assign">
                  <Roles />
                </RequireAuthority>
              }
            />
            <Route
              path="roles/new"
              element={
                <RequireAuthority authority="roles:manage">
                  <RoleEditor />
                </RequireAuthority>
              }
            />
            <Route
              path="roles/:id"
              element={
                <RequireAuthority authority="roles:assign">
                  <RoleEditor />
                </RequireAuthority>
              }
            />
          </Route>
          <Route
            path={paths.platform}
            element={
              <RequireAuthority authority="workspaces:read">
                <PlatformSection />
              </RequireAuthority>
            }
          >
            <Route index element={<Navigate to={paths.workspaces} replace />} />
            <Route path="workspaces" element={<Workspaces />} />
            <Route
              path="workspaces/new"
              element={
                <RequireAuthority authority="workspaces:create">
                  <WorkspaceEditor />
                </RequireAuthority>
              }
            />
            <Route
              path="workspaces/:id"
              element={
                <RequireAuthority authority="workspaces:update">
                  <WorkspaceEditor />
                </RequireAuthority>
              }
            />
            <Route path="defaults" element={<PlatformDefaults />} />
          </Route>
          {/* Where these used to be, so a bookmark or an old link still lands. */}
          <Route path="/users/*" element={<Moved from="/users" to={paths.users} />} />
          <Route path="/roles/*" element={<Moved from="/roles" to={paths.roles} />} />
          <Route
            path="/workspaces/*"
            element={<Moved from="/workspaces" to={paths.workspaces} />}
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
      <Logo className="h-8 w-8 animate-pulse" />
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
