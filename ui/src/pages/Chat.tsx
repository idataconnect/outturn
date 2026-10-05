import { useEffect, useMemo, useRef, useState } from 'react'
import { Link, NavLink, useNavigate, useParams, useSearchParams } from 'react-router'
import { AssistantRuntimeProvider } from '@assistant-ui/react'
import { Menu, PanelLeftClose, Paperclip, Plus, X } from 'lucide-react'

import { useSkillCommands } from '../lib/useSkillCommands'

import ApprovalPrompt from '../components/ApprovalPrompt'
import SleepBanner from '../components/SleepBanner'
import Thread from '../components/Thread'
import PromptBanner from '../components/PromptBanner'
import AgentList from '../components/AgentList'
import FilesPanel from '../components/FilesPanel'
import SidePane, { type PaneTab } from '../components/SidePane'
import { ApiError } from '../lib/api'
import {
  createSession,
  listAgents,
  listSessions,
  mergeRecent,
  recentSessions,
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
import { iconButtonLarge } from '../lib/buttons'

/**
 * Conversations: the list, the one open, and -- under `/sessions/new` -- one
 * about to be started. `?agent=` names who it will be with; without it the
 * page asks. Nothing is made until the first message is sent.
 */
export default function Chat({ draft = false }: { draft?: boolean }) {
  const state = useSession()
  const navigate = useNavigate()
  // The URL owns the selection, so a session can be linked to, reloaded, and
  // reached with the back button.
  const { sessionId } = useParams<{ sessionId?: string }>()
  const [search] = useSearchParams()
  const draftAgent = draft ? search.get('agent') : null
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
  /** Filled by the composer; read by the runtime as a message is sent. */
  const takeAttachments = useRef<(() => string) | null>(null)

  // Who a new chat could be with: the agents this reader may start one with
  // and that will answer, which the API says rather than this page guessing.
  const chattable = useMemo(() => agents.filter((a) => a.can_chat), [agents])
  // Who this new chat is with: the one the URL names, or -- when there is only
  // one to have -- that one, since a choice of one is not worth asking.
  const draftWith = !draft
    ? undefined
    : draftAgent
      ? chattable.find((a) => a.id === draftAgent)
      : chattable.length === 1
        ? chattable[0]
        : undefined

  const draftWithId = draftWith?.id
  const fresh = useMemo(
    () =>
      draftWithId
        ? {
            agent: draftWithId,
            create: async () => {
              const session = await createSession(draftWithId)
              setSessions((prev) => [session, ...prev])
              return session.id
            },
            // Replaced rather than pushed: back from the conversation should
            // not land on an empty "new" page for a chat that now exists.
            opened: (id: string) => void navigate(`/sessions/${id}`, { replace: true }),
          }
        : undefined,
    [draftWithId, navigate],
  )

  /** Bumped when the composer or the agent stores or removes a file, so the files panel
   *  shows what happened without anyone reopening it. */
  const [storedChange, setStoredChange] = useState(0)

  const {
    runtime,
    error: chatError,
    held,
    stopping,
    compacting,
    compactions,
    retry,
    clearHeld,
    dismissError,
  } = useChatRuntime(
    active,
    (title) => {
      if (!active) return
      setSessions((prev) => prev.map((s) => (s.id === active ? { ...s, title } : s)))
    },
    // Read as a message is sent. Held in a ref filled by the composer, which
    // is where the attachments are: the runtime is created here, and the two
    // would otherwise have no way to meet.
    () => takeAttachments.current?.() ?? '',
    fresh,
    // What the agent stores lands in the same panel as what the composer
    // does, so it is told the same way.
    () => setStoredChange((n) => n + 1),
  )

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
  // Starting a conversation and saying something in one are separate grants,
  // and a viewer holds neither. Both were checked only by the API, so the
  // button was offered and threw, and the composer took a message that went
  // nowhere -- the worse of the two, because somebody who types a paragraph
  // and watches it vanish has lost work rather than been refused.
  const canStart =
    state.status === 'authenticated' &&
    state.session.authorities.includes('sessions:create')
  const canSend =
    state.status === 'authenticated' &&
    state.session.authorities.includes('gateway:invoke')
  // Whether to offer the approval in the conversation at all. The API refuses
  // anyone without it, so showing the buttons is safe -- but a clerk who presses
  // Approve and is told 403 has been invited to do something they cannot, which
  // is worse than not being asked. `Holds` draws the same line with
  // `canRelease`.
  const canAnswerApprovals =
    state.status === 'authenticated' &&
    state.session.authorities.includes('approvals:answer')
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
  // Bumped by choosing a session, new or existing: whoever just picked a
  // conversation is about to type into it.
  const [focusRequest, setFocusRequest] = useState(0)

  function onStoredChange(failure?: string) {
    if (failure) {
      setError(failure)
      return
    }
    setStoredChange((n) => n + 1)
    setError(null)
  }

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

  // The list says which conversations are working, so it has to keep up with
  // them. There is no workspace-wide event stream to listen to, so it asks
  // again every few seconds while the tab is visible. One page, merged over
  // what is held: re-reading the whole list grew with every session ever made.
  //
  // An answer arriving after the workspace changed is dropped: merged, it
  // would put one workspace's conversations into another's list.
  useEffect(() => {
    let current = true
    const id = setInterval(() => {
      if (document.visibilityState !== 'visible') return
      recentSessions().then(
        (fresh) => {
          if (current) setSessions((held) => mergeRecent(held, fresh))
        },
        // A missed refresh keeps the list as it was; the next one retries.
        () => {},
      )
    }, SESSIONS_REFRESH_MS)
    return () => {
      current = false
      clearInterval(id)
    }
  }, [workspaceId])

  // Stale the instant the workspace changes, so the reconciliation below waits
  // for the new lists rather than judging the URL against the old ones.
  const loaded = loadedFor !== null && loadedFor === workspaceId

  // Reconcile the URL against what this workspace can actually see, once loaded.
  useEffect(() => {
    // A new chat names no session, so there is nothing to reconcile.
    if (!loaded || draft) return

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
    //
    // Straight to where it lands, rather than by way of /sessions: that hop
    // was decided a render later, and somebody who clicked something in
    // between had their choice replaced by the landing.
    if (!sessions.some((session) => session.id === sessionId)) {
      setMissing('That session is not available in this workspace.')
      if (sessions.length > 0) {
        landed.current = sessions[0].id
        void navigate(`/sessions/${sessions[0].id}`, { replace: true })
      } else {
        void navigate('/sessions', { replace: true })
      }
      return
    }
    // The notice explains the landing it caused; it should not still be
    // there once a session the reader chose is on screen.
    if (sessionId !== landed.current) setMissing(null)
  }, [loaded, draft, sessionId, sessions, navigate])

  // Whoever just chose who to talk to is about to type to them.
  useEffect(() => {
    if (draftWith) setFocusRequest((n) => n + 1)
  }, [draftWith])

  const agentName = (id: string) => agents.find((a) => a.id === id)?.name ?? 'Agent'
  // A notice about a link that went nowhere is about the page that link
  // landed on, and a new chat is not it.
  const shown = error ?? chatError ?? (draft ? null : missing)

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
              render: () => <FilesPanel sessionId={active} reloadKey={storedChange} />,
            },
          ]
        : [],
    // `storedChange` too: the render closes over it, so a panel built before
    // an upload would keep showing the list from before it.
    [active, storedChange],
  )

  const current = sessions.find((s) => s.id === active)
  // What the composer's `/` menu offers. Keyed on the open session's agent,
  // so switching sessions switches the menu with it.
  const skills = useSkillCommands(current?.agent_id ?? draftWithId ?? null)
  const activeTitle = active
    ? sessionName(current)
    : draftWith
      ? `New chat with ${draftWith.name}`
      : draft
        ? 'New chat'
        : 'Sessions'

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
        {/* No close button of its own. The header's toggle already hides this
            panel and is the only thing that can bring it back, so a second
            control that could only do half the job left the pair disagreeing
            about which one to reach for -- and it sat inside the thing it
            closed, vanishing with it. */}
        {/* One button, not a button per agent. The list of agents used to sit
            here, and it grew with the workspace until the conversations it
            sat above were pushed off the bottom of the panel. Choosing who to
            talk to is the new-chat page's job. */}
        <div className="p-3 border-b border-surface-200 dark:border-surface-800">
          {agents.length === 0 ? (
            canCreateAgents ? (
              <NavLink
                to="/agents/new"
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
          ) : canStart && chattable.length > 0 ? (
            <Link
              to="/sessions/new"
              onClick={() => {
                if (breakpoint === 'phone') toggleSessions(false)
              }}
              className="flex items-center justify-center gap-2 px-3 py-1.5 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium"
            >
              <Plus size={14} aria-hidden />
              New chat
            </Link>
          ) : (
            // A badge rather than a sentence. What a reader needs is to know
            // that no button is the arrangement rather than a list that failed
            // to load, and two words do that. The reason is in the tooltip for
            // whoever is asking why.
            <span
              title={
                canStart
                  ? 'You have not been given any agent to start a chat with'
                  : 'Starting a chat needs the sessions:create authority'
              }
              className="inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-medium uppercase tracking-wide bg-surface-100 dark:bg-surface-800 text-surface-500 dark:text-surface-400"
            >
              Read-only
            </span>
          )}
        </div>
        <div className="flex-1 overflow-auto p-2 space-y-1">
          {sessions.map((session) => (
            <NavLink
              key={session.id}
              to={`/sessions/${session.id}`}
              // Picking a session dismisses the list only where the list was
              // in the way. Inline it sits beside the thread, and closing it
              // would take the sidebar away every time somebody used it.
              onClick={() => {
                if (breakpoint === 'phone') toggleSessions(false)
                setFocusRequest((n) => n + 1)
              }}
              title={`${sessionName(session)} — ${agentName(session.agent_id)}`}
              className={({ isActive }) =>
                `block w-full px-2 py-1.5 rounded-md text-sm text-left ${
                  isActive
                    ? 'bg-brand-50 dark:bg-brand-950 text-brand-800 dark:text-brand-200'
                    : 'text-surface-600 dark:text-surface-400 hover:bg-surface-50 dark:hover:bg-surface-800/50'
                }`
              }
            >
              <span className="flex items-center gap-1.5">
                <span className="truncate">{sessionName(session)}</span>
                <TurnMark turn={session.turn} />
              </span>
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
          {(current || draftWith) && (
            <Link
              to={`/agents/${current?.agent_id ?? draftWith?.id}`}
              className="hidden sm:block shrink-0 text-xs text-surface-400 dark:text-surface-500 hover:underline underline-offset-2"
            >
              {agentName(current?.agent_id ?? draftWith!.id)}
            </Link>
          )}
        </div>
        {shown && (
          <div
            className="flex items-start gap-3 px-6 py-2 text-sm text-red-600 dark:text-red-400 border-b border-surface-200 dark:border-surface-800"
            role="alert"
          >
            <p className="min-w-0 flex-1">{shown}</p>
            {/* An error can be put away; a link that went nowhere cannot, since
                it is about the page itself and leaves with it. */}
            {(error ?? chatError) && (
              <button
                type="button"
                onClick={() => {
                  setError(null)
                  dismissError()
                }}
                aria-label="Dismiss"
                title="Dismiss"
                className="shrink-0 rounded p-0.5 text-red-500 hover:bg-red-50 hover:text-red-700 dark:text-red-400 dark:hover:bg-red-950/40 dark:hover:text-red-300"
              >
                <X size={14} aria-hidden />
              </button>
            )}
          </div>
        )}
        {/* A hold reads as a pause, not a fault: nothing was lost and nothing
            is being retried. `status` rather than `alert` for the same reason
            -- a screen reader should hear this as the state of the
            conversation, not as something going wrong. */}
        {held && !shown && (
          // Tinted, so the amber card has something to sit against. On the
          // page's own background the card and the strip around it read as one
          // shape, and the band that is holding the conversation up looks like
          // part of the transcript rather than something across it.
          <div className="border-b border-surface-200 bg-surface-100 px-6 py-2 dark:border-surface-800 dark:bg-surface-800">
            {/* A sleep is a pause the agent chose, not a fault or a question,
                so it is not drawn in the amber of one. Anybody who may send
                here may end it: sending is what it holds back. */}
            {held.asleep && active ? (
              <SleepBanner sessionId={active} asleep={held.asleep} onWoken={clearHeld} />
            ) : (
              <p className="text-sm text-amber-700 dark:text-amber-400" role="status">
                {/* "Hold" is a word from inside this platform, and the sentence
                    was also passive about something the reader is often the one
                    to do. Said as what happens next, to them. */}
                {held.message}
                {held.resumable
                  ? held.approval
                    ? ' \u2014 it will carry on as soon as somebody answers.'
                    : ' \u2014 it will carry on by itself once this is sorted.'
                  : ' \u2014 send a message to pick it up again once this is sorted.'}
              </p>
            )}
            {/* Answerable here when the hold is an approval. The queue remains
                the place to find every pending decision; this is for the person
                who was already looking -- and whether they may answer is the
                API's to say, not this component's. */}
            {held.approval && canAnswerApprovals && (
              <ApprovalPrompt
                approval={held.approval}
                onAnswered={() => {
                  // The turn is given back to the queue by the answer itself,
                  // so nothing here restarts it. Clearing the banner is all
                  // that is owed: the reply resumes streaming on its own.
                  clearHeld()
                }}
              />
            )}
          </div>
        )}
        {active && current && (
          <PromptBanner
            key={active}
            sessionId={active}
            agentId={current.agent_id}
            compacting={compacting}
            compactions={compactions}
          />
        )}
        <div className="flex-1 min-h-0">
          {/* A new chat draws nothing until the agents are in: whether to ask
              who with depends on how many there are, and a guess either way
              is drawn and then taken back. */}
          {draft && !loaded ? null : draft && !draftWith ? (
            // Who to talk to, asked in the page rather than in a menu: this is
            // the whole of what the reader came here to decide.
            <div className="h-full overflow-auto p-6">
              <div className="max-w-md mx-auto">
                <h2 className="text-lg font-semibold text-surface-900 dark:text-surface-100">
                  Who would you like to talk to?
                </h2>
                {draftAgent && loaded && (
                  <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
                    That agent is not one you can start a chat with. Pick another.
                  </p>
                )}
                <div className="mt-4">
                  <AgentList
                    agents={chattable}
                    href={(agent) => `/sessions/new?agent=${agent.id}`}
                    autoFocus
                    empty={
                      <p className="text-sm text-surface-600 dark:text-surface-400">
                        There is no agent you can start a chat with.
                      </p>
                    }
                  />
                </div>
              </div>
            </div>
          ) : (
          <AssistantRuntimeProvider runtime={runtime}>
            <Thread
              disabled={(!active && !draftWith) || !canSend}
              readOnly={!!active && !canSend}
              skills={skills}
              onRetry={retry}
              stopping={stopping}
              focusRequest={focusRequest}
              sessionId={active}
              onStoredChange={onStoredChange}
              takeAttachments={takeAttachments}
            />
          </AssistantRuntimeProvider>
          )}
        </div>
      </div>

      {active && <SidePane tabs={paneTabs} storageKey="chat.pane" />}
    </div>
  )
}

/** How often the session list is read again for order and activity. */
const SESSIONS_REFRESH_MS = 5_000

/**
 * Whether a conversation has a turn in flight, beside its name.
 *
 * Working (running or queued) pulses in the brand colour; waiting on a person
 * is amber and still, since nothing will happen until somebody answers. Idle
 * draws nothing, which is most of the list.
 */
function TurnMark({ turn }: { turn: AgentSession['turn'] }) {
  if (!turn) return null
  const held = turn === 'parked'
  const label = held ? 'Waiting on approval' : turn === 'running' ? 'Working' : 'Queued'
  return (
    <span
      role="status"
      aria-label={label}
      title={label}
      className={`ml-auto h-2 w-2 shrink-0 rounded-full ${
        held ? 'bg-amber-500' : 'animate-pulse bg-brand-500'
      }`}
    />
  )
}
