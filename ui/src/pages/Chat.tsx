import { useEffect, useMemo, useRef, useState } from 'react'
import { NavLink, useNavigate, useParams } from 'react-router'
import { AssistantRuntimeProvider } from '@assistant-ui/react'
import { Menu, PanelLeftClose, Paperclip, Plus, X } from 'lucide-react'

import Thread from '../components/Thread'
import FilesPanel from '../components/FilesPanel'
import SidePane, { type PaneTab } from '../components/SidePane'
import { ApiError } from '../lib/api'
import {
  createSession,
  listAgents,
  listSessions,
  renameSession,
  sessionName,
  type Agent,
  type AgentSession,
} from '../lib/chat'
import SessionTitle from '../components/SessionTitle'
import { useChatRuntime } from '../lib/useChatRuntime'
import { useSession } from '../lib/session'
import { readFlag, storeFlag } from '../lib/layout'
import { currentBreakpoint, useBreakpoint } from '../lib/useBreakpoint'
import { iconButton, iconButtonLarge } from '../lib/buttons'

export default function Chat() {
  const state = useSession()
  const navigate = useNavigate()
  // The URL owns the selection, so a session can be linked to, reloaded, and
  // reached with the back button.
  const { sessionId } = useParams<{ sessionId?: string }>()
  const [agents, setAgents] = useState<Agent[]>([])
  const [sessions, setSessions] = useState<AgentSession[]>([])
  const [error, setError] = useState<string | null>(null)
  // A URL naming a session this workspace cannot see. Kept apart from
  // `error` because it is about the address, not the page: it goes away the
  // moment a session that exists is shown, where a failed request would not.
  const [missing, setMissing] = useState<string | null>(null)
  /** Where a bad URL was redirected to, so the notice outlives that landing. */
  const landed = useRef<string | null>(null)

  const active = sessionId ?? null
  // A title arriving over the feed -- the namer's, after the first turn, or
  // a rename from another tab -- lands in the list the sidebar draws from.
  const { runtime, error: chatError, stopping } = useChatRuntime(active, (title) => {
    if (!active) return
    setSessions((prev) => prev.map((s) => (s.id === active ? { ...s, title } : s)))
  })

  // Agents and sessions are workspace-scoped, so switching workspace reloads both.
  // Selecting a session does not: that only changes which one is shown.
  const workspaceId = state.status === 'authenticated' ? state.session.workspace_id : null
  // Offered as a way out only to somebody who could act on it. Sending a
  // reader to a page where they can look but not create leaves them exactly
  // where they started, having been told to do something they cannot.
  const canCreateAgents =
    state.status === 'authenticated' &&
    state.session.authorities.includes('agents:create')
  const canRename =
    state.status === 'authenticated' &&
    state.session.authorities.includes('sessions:update')
  // Which workspace the lists on screen describe, or null before the first
  // load finishes. Held as the workspace rather than a bare flag so that
  // "loaded" can be derived during render: a switch makes it stale the moment
  // it happens, with no effect needed to reset it first.
  const [loadedFor, setLoadedFor] = useState<string | null>(null)
  // The sessions list can be put away at any width, not only when the window
  // forces it: on a wide screen it is 256px that a reader deep in one
  // conversation may would rather give to the thread. The width only picks the
  // default, and only a phone defaults to closed.
  const breakpoint = useBreakpoint()
  const [sessionsOpen, setSessionsOpen] = useState(() =>
    readFlag('chat.sessions', currentBreakpoint() !== 'phone'),
  )

  function toggleSessions(next: boolean) {
    setSessionsOpen(next)
    storeFlag('chat.sessions', next)
  }

  useEffect(() => {
    let cancelled = false
    void (async () => {
      try {
        const [a, s] = await Promise.all([listAgents(), listSessions()])
        if (cancelled) return
        setAgents(a)
        setSessions(s)
        setError(null)
      } catch (e) {
        if (cancelled) return
        setError(e instanceof ApiError ? e.message : 'failed to load')
      } finally {
        if (!cancelled) setLoadedFor(workspaceId)
      }
    })()
    return () => {
      cancelled = true
    }
  }, [workspaceId])

  // Stale the instant the workspace changes, so the reconciliation below waits
  // for the new lists rather than judging the URL against the old ones.
  const loaded = loadedFor !== null && loadedFor === workspaceId

  // Reconcile the URL against what this workspace can actually see, once loaded.
  useEffect(() => {
    if (!loaded) return

    // Land on the most recent session when none was named. Replace rather than
    // push, so the back button does not return to an empty /sessions that
    // immediately redirects here again.
    if (!sessionId) {
      if (sessions.length > 0) {
        landed.current = sessions[0].id
        void navigate(`/sessions/${sessions[0].id}`, { replace: true })
      }
      return
    }

    // A session in the URL this workspace cannot see -- a stale link, or one left
    // behind by a workspace switch -- would otherwise leave a blank pane with no
    // explanation.
    if (!sessions.some((session) => session.id === sessionId)) {
      setMissing('That session is not available in this workspace.')
      void navigate('/sessions', { replace: true })
      return
    }
    // The notice explains the landing it caused; it should not still be
    // there once a session the reader chose is on screen.
    if (sessionId !== landed.current) setMissing(null)
  }, [loaded, sessionId, sessions, navigate])

  async function start(agentId: string) {
    try {
      const session = await createSession(agentId)
      setSessions((prev) => [session, ...prev])
      void navigate(`/sessions/${session.id}`)
      setError(null)
      toggleSessions(false)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to start session')
    }
  }

  const agentName = (id: string) => agents.find((a) => a.id === id)?.name ?? 'Agent'
  const shown = error ?? chatError ?? missing

  // `render` closes over the session id, so the tab list is rebuilt only when
  // that changes; a new component identity on every render would remount the
  // panel and throw away whatever it had loaded.
  const paneTabs = useMemo<PaneTab[]>(
    () =>
      active
        ? [
            {
              id: 'files',
              label: 'Files',
              icon: Paperclip,
              render: () => <FilesPanel sessionId={active} />,
            },
          ]
        : [],
    [active],
  )

  const current = sessions.find((s) => s.id === active)
  const activeTitle = active ? sessionName(current) : 'Sessions'

  async function rename(id: string, title: string) {
    try {
      const updated = await renameSession(id, title)
      setSessions((prev) => prev.map((s) => (s.id === id ? updated : s)))
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to rename session')
    }
  }

  return (
    <div className="flex h-full relative">
      <aside
        className={`${sessionsOpen ? 'flex' : 'hidden'} flex-col ${
          breakpoint === 'phone' ? 'fixed inset-y-0 left-0 shadow-xl' : 'static'
        } z-30 w-64 shrink-0 border-r border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900`}
      >
        <div className="p-3 border-b border-surface-200 dark:border-surface-800 flex items-center justify-between gap-2">
          <p className="text-xs font-medium text-surface-600 dark:text-surface-400">
            Start a session
          </p>
          <button
            type="button"
            onClick={() => toggleSessions(false)}
            aria-label="Close sessions"
            className={iconButton}
          >
            <X size={16} aria-hidden />
          </button>
        </div>
        <div className="p-3 border-b border-surface-200 dark:border-surface-800">
          {agents.length === 0 ? (
            canCreateAgents ? (
              <NavLink
                to="/agents"
                className="flex items-center gap-2 px-2 py-1.5 rounded-md text-sm text-brand-700 dark:text-brand-400 hover:bg-surface-100 dark:hover:bg-surface-800"
              >
                <Plus size={14} className="shrink-0" />
                <span>Create an agent</span>
              </NavLink>
            ) : (
              <p className="text-xs text-surface-600 dark:text-surface-400">
                No agents yet. Ask an administrator to add one.
              </p>
            )
          ) : (
            <div className="space-y-1">
              {agents.map((agent) => (
                <button
                  key={agent.id}
                  onClick={() => void start(agent.id)}
                  className="w-full flex items-center gap-2 px-2 py-1.5 rounded-md text-sm text-left text-surface-700 dark:text-surface-300 hover:bg-surface-100 dark:hover:bg-surface-800"
                >
                  <Plus size={14} className="shrink-0 text-surface-400" />
                  <span className="truncate">{agent.name}</span>
                </button>
              ))}
            </div>
          )}
        </div>
        <div className="flex-1 overflow-auto p-2 space-y-1">
          {sessions.map((session) => (
            <NavLink
              key={session.id}
              to={`/sessions/${session.id}`}
              onClick={() => setSessionsOpen(false)}
              title={`${sessionName(session)} — ${agentName(session.agent_id)}`}
              className={({ isActive }) =>
                `block w-full px-2 py-1.5 rounded-md text-sm text-left ${
                  isActive
                    ? 'bg-brand-50 dark:bg-brand-950 text-brand-800 dark:text-brand-200'
                    : 'text-surface-600 dark:text-surface-400 hover:bg-surface-50 dark:hover:bg-surface-800/50'
                }`
              }
            >
              <span className="block truncate">{sessionName(session)}</span>
              <span className="block truncate text-xs text-surface-400 dark:text-surface-500">
                {agentName(session.agent_id)}
              </span>
            </NavLink>
          ))}
        </div>
      </aside>

      {sessionsOpen && breakpoint === 'phone' && (
        <div
          className="fixed inset-0 z-20 bg-black/30"
          onClick={() => toggleSessions(false)}
          aria-hidden
        />
      )}

      <div className="flex-1 flex flex-col min-w-0">
        {/* One header at every width. The pair this replaces -- one below
            `lg`, one above -- had drifted apart, which is why the sessions
            toggle existed on a phone and nowhere else. */}
        <div className="flex items-center gap-2 px-2 lg:px-4 py-2 border-b border-surface-200 dark:border-surface-800">
          <button
            type="button"
            onClick={() => toggleSessions(!sessionsOpen)}
            aria-label={sessionsOpen ? 'Hide sessions' : 'Show sessions'}
            aria-expanded={sessionsOpen}
            title={sessionsOpen ? 'Hide sessions' : 'Show sessions'}
            className={iconButtonLarge}
          >
            {sessionsOpen ? <PanelLeftClose size={18} aria-hidden /> : <Menu size={18} aria-hidden />}
          </button>
          <SessionTitle
            title={activeTitle}
            canRename={canRename && !!current}
            onRename={(t) => current && void rename(current.id, t)}
          />
          {current && (
            <span className="hidden sm:block shrink-0 text-xs text-surface-400 dark:text-surface-500">
              {agentName(current.agent_id)}
            </span>
          )}
        </div>
        {shown && (
          <p
            className="px-6 py-2 text-sm text-red-600 dark:text-red-400 border-b border-surface-200 dark:border-surface-800"
            role="alert"
          >
            {shown}
          </p>
        )}
        <div className="flex-1 min-h-0">
          <AssistantRuntimeProvider runtime={runtime}>
            <Thread disabled={!active} stopping={stopping} />
          </AssistantRuntimeProvider>
        </div>
      </div>

      {active && <SidePane tabs={paneTabs} storageKey="chat.pane" />}
    </div>
  )
}
