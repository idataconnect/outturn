import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router'
import { Hand } from 'lucide-react'

import { agentActivity, listAgents, type Agent, type AgentActivity as Activity } from '../lib/chat'
import { elapsedPhrase } from '../lib/elapsed'
import { useSession } from '../lib/session'

/** How often the panel asks again: the sessions sidebar's cadence. */
const REFRESH_MS = 5_000

/** Idle agents listed before the rest are folded into a count. */
const IDLE_SHOWN = 5

type State = 'working' | 'waiting' | 'idle'

type Row = {
  agent: Agent
  state: State
  running: number
  queued: number
  waiting: number
  /** The live conversation to open: the most recently active one. */
  open?: string
  lastActive?: number
}

const ORDER: Record<State, number> = { working: 0, waiting: 1, idle: 2 }

/**
 * Which agents are doing something right now.
 *
 * Read from `/v1/agents/activity`, which says what each agent's live turns are doing --
 * not from the usage ledger like the rest of this page, which is about a
 * window rather than this moment. So it sits above the window's figures and
 * ignores the picker.
 *
 * An agent is *working* when any of its conversations has a turn running or
 * queued, *waiting* when its only live turns are parked on an approval, and
 * idle otherwise. Disabled agents are left out: they cannot be anything else.
 */
export default function AgentActivity() {
  const session = useSession()
  // Asked only of somebody who may read both halves: the activity is about
  // sessions and the names come from agents, and a page should not make
  // requests it knows will be refused.
  const authorities = session.status === 'authenticated' ? session.session.authorities : []
  const canRead = authorities.includes('sessions:read') && authorities.includes('agents:read')
  const [agents, setAgents] = useState<Agent[] | null>(null)
  const [activity, setActivity] = useState<Activity[]>([])
  const [now, setNow] = useState(() => Date.now())

  // Names once, activity every few seconds: the agents change when somebody
  // edits one, and the activity is one row per agent answered from indexes
  // sized by what is in flight -- see `agent_activity` in the API.
  useEffect(() => {
    if (!canRead) return
    let current = true
    const load = async () => {
      try {
        const a = await agentActivity()
        if (!current) return
        setActivity(Array.isArray(a) ? a : [])
        setNow(Date.now())
      } catch {
        // Kept as it was rather than emptied: emptied, a failing endpoint
        // read exactly like a workspace with no agents. What is shown stays
        // until a read succeeds, and the next tick tries again.
      }
    }
    void (async () => {
      try {
        const a = await listAgents()
        if (current) setAgents(a)
      } catch {
        if (current) setAgents([])
      }
    })()
    void load()
    const id = setInterval(() => {
      if (document.visibilityState === 'visible') void load()
    }, REFRESH_MS)
    return () => {
      current = false
      clearInterval(id)
    }
  }, [canRead])

  const rows = useMemo(() => rowsOf(agents ?? [], activity), [agents, activity])

  if (!canRead || !agents || rows.length === 0) return null

  const count = { working: 0, waiting: 0, idle: 0 }
  for (const r of rows) count[r.state]++
  // Everything live is listed; idle agents only up to a handful, since a
  // workspace with forty agents wants to see the four doing something.
  const listed = rows.filter((r, i) => r.state !== 'idle' || i < count.working + count.waiting + IDLE_SHOWN)
  const hidden = rows.length - listed.length
  const inFlight = rows.reduce((n, r) => n + r.running + r.queued, 0)

  return (
    <section
      aria-labelledby="agent-activity"
      className="rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 p-5"
    >
      <div className="flex items-start justify-between gap-4">
        <div>
          <h2
            id="agent-activity"
            className="text-xs font-medium uppercase tracking-wide text-surface-500 dark:text-surface-400"
          >
            Right now
          </h2>
          <p className="mt-1 text-3xl font-semibold text-surface-900 dark:text-surface-100">
            {count.working}
            <span className="text-surface-400 dark:text-surface-500 font-normal">
              {' '}
              / {rows.length}
            </span>
            {' '}
            <span className="ml-1 text-base font-medium text-surface-600 dark:text-surface-300">
              {count.working === 1 ? 'agent working' : 'agents working'}
            </span>
          </p>
          <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
            {inFlight === 0
              ? 'Nothing in flight.'
              : `${inFlight} ${inFlight === 1 ? 'turn' : 'turns'} in flight`}
            {count.waiting > 0 &&
              ` · ${count.waiting} waiting on approval`}
          </p>
        </div>
        {/* Says the figures move by themselves, so a reader does not reload
            to find out whether they are current. */}
        <span className="flex items-center gap-1.5 text-xs text-surface-500 dark:text-surface-400">
          <span className="relative flex h-2 w-2">
            <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-brand-500 opacity-60 motion-reduce:hidden" />
            <span className="relative inline-flex h-2 w-2 rounded-full bg-brand-500" />
          </span>
          Live
        </span>
      </div>

      {/* One segment per agent, in the list's order: proportion at a glance,
          and the segments line up with the rows below them. */}
      <div className="mt-4 flex h-2.5 gap-0.5" role="img" aria-label={summary(count)}>
        {rows.map((r) => (
          <div
            key={r.agent.id}
            title={`${r.agent.name}: ${label(r)}`}
            className={`flex-1 first:rounded-l-full last:rounded-r-full transition-colors duration-500 ${SEGMENT[r.state]}`}
          />
        ))}
      </div>
      <ul className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-xs text-surface-600 dark:text-surface-400">
        <Legend dot={SEGMENT.working} text={`${count.working} working`} />
        {count.waiting > 0 && <Legend dot={SEGMENT.waiting} text={`${count.waiting} waiting`} />}
        <Legend dot={SEGMENT.idle} text={`${count.idle} idle`} />
      </ul>

      <ul className="mt-4 divide-y divide-surface-100 dark:divide-surface-800">
        {listed.map((r) => (
          <li key={r.agent.id} className="flex items-center gap-3 py-2 text-sm">
            <Dot state={r.state} />
            <span
              className={`min-w-0 flex-1 truncate ${
                r.state === 'idle'
                  ? 'text-surface-600 dark:text-surface-400'
                  : 'font-medium text-surface-900 dark:text-surface-100'
              }`}
            >
              {r.agent.name}
            </span>
            <span className="shrink-0 text-xs tabular-nums text-surface-500 dark:text-surface-400">
              {r.state === 'idle'
                ? r.lastActive
                  ? `Idle · ${elapsedPhrase(r.lastActive, now).toLowerCase()}`
                  : 'Idle · never used'
                : label(r)}
            </span>
            {r.open && (
              <Link
                to={`/sessions/${r.open}`}
                className="shrink-0 text-xs font-medium text-brand-700 hover:underline dark:text-brand-400"
              >
                Open
              </Link>
            )}
          </li>
        ))}
        {hidden > 0 && (
          <li className="py-2 text-xs text-surface-500 dark:text-surface-400">
            and {hidden} more idle
          </li>
        )}
      </ul>
    </section>
  )
}

const SEGMENT: Record<State, string> = {
  working: 'bg-brand-500',
  waiting: 'bg-amber-500',
  idle: 'bg-surface-200 dark:bg-surface-700',
}

function Dot({ state }: { state: State }) {
  if (state === 'waiting')
    // Status is never color alone: the hand says "a person is needed".
    return <Hand size={12} className="shrink-0 text-amber-600 dark:text-amber-400" aria-label="Waiting" />
  return (
    <span
      aria-label={state === 'working' ? 'Working' : 'Idle'}
      className={`h-2 w-2 shrink-0 rounded-full ${
        state === 'working' ? 'animate-pulse bg-brand-500' : 'bg-surface-300 dark:bg-surface-600'
      }`}
    />
  )
}

function Legend({ dot, text }: { dot: string; text: string }) {
  return (
    <li className="flex items-center gap-1.5">
      <span className={`h-2 w-2 rounded-full ${dot}`} aria-hidden />
      {text}
    </li>
  )
}

function label(r: Row): string {
  const parts = []
  if (r.running) parts.push(`${r.running} running`)
  if (r.queued) parts.push(`${r.queued} queued`)
  if (r.waiting) parts.push(`${r.waiting} waiting`)
  return parts.length ? parts.join(' · ') : 'Idle'
}

function summary(c: Record<State, number>): string {
  return `${c.working} working, ${c.waiting} waiting on approval, ${c.idle} idle`
}

function rowsOf(agents: Agent[], activity: Activity[]): Row[] {
  const rows = new Map<string, Row>()
  // Only agents the activity reports on: it already leaves out disabled ones
  // and those outside a narrowed reader's reach.
  const named = new Map(agents.map((a) => [a.id, a]))
  for (const a of activity) {
    const agent = named.get(a.agent_id)
    if (!agent) continue
    rows.set(agent.id, {
      agent,
      state: 'idle',
      running: a.running,
      queued: a.queued,
      waiting: a.waiting,
      open: a.live_session_id ?? undefined,
      lastActive: a.last_active_at ? Date.parse(a.last_active_at) : undefined,
    })
  }
  for (const row of rows.values()) {
    row.state = row.running + row.queued > 0 ? 'working' : row.waiting > 0 ? 'waiting' : 'idle'
  }
  return [...rows.values()].sort(
    (a, b) =>
      ORDER[a.state] - ORDER[b.state] ||
      (b.lastActive ?? 0) - (a.lastActive ?? 0) ||
      a.agent.name.localeCompare(b.agent.name),
  )
}
