import { AlertTriangle, Clock, PauseCircle } from 'lucide-react'

import { compact, exact } from '../lib/viz'
import StatTile from './StatTile'

export type SkillStats = {
  from: string
  to: string
  totals: {
    skills: number
    bound: number
    versions: number
    created: number
    retired: number
    turns: number
  }
  used: {
    skill_id: string
    name: string
    slug: string
    turns: number
    sessions: number
    versions: number
    last_used: string | null
  }[]
  idle: {
    skill_id: string
    name: string
    slug: string
    agents: number
    last_used: string | null
  }[]
  lagging: {
    skill_id: string
    name: string
    slug: string
    latest: number
    serving: number
    turns: number
    pinned: boolean
  }[]
  authors: {
    user_id: string | null
    name: string | null
    versions: number
    skills: number
  }[]
}

/** How long ago, in the roughest terms that are still true. */
function since(iso: string | null): string {
  if (iso === null) return 'never'
  const days = Math.floor((Date.now() - new Date(iso).getTime()) / 86_400_000)
  if (days <= 0) return 'today'
  if (days === 1) return 'yesterday'
  if (days < 30) return `${days} days ago`
  const months = Math.round(days / 30)
  return months === 1 ? 'a month ago' : `${months} months ago`
}

/**
 * What the workspace's skills are doing.
 *
 * Deliberately not part of the usage panels above it: those read the ledger and
 * are about spend, these read the transcript and are about what was carried.
 * Mixing them would make one window look like it covered everything.
 *
 * The two panels worth the space are the ones nobody can ask for otherwise. A
 * skill's body is paid for on every round of every turn it is bound to, so an
 * idle skill is a standing cost with nothing to show; and a skill serving an
 * old version is either a pin doing its job or an edit that never took effect,
 * which is the "I fixed it and it still does the old thing" confusion.
 */
export default function SkillStats({ stats }: { stats: SkillStats }) {
  const { totals } = stats
  const busiest = stats.used[0]?.turns ?? 0

  return (
    <section className="space-y-4">
      <div>
        <h2 className="text-sm font-semibold text-surface-900 dark:text-surface-100">Skills</h2>
        <p className="text-xs text-surface-500 dark:text-surface-400">
          What the agents were carrying, and what it did. Read from the
          transcript rather than the usage ledger, so these are turns rather
          than tokens.
        </p>
      </div>

      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <StatTile label="Skills" value={totals.skills} hint={`${totals.bound} carried by an agent`} />
        <StatTile label="Turns served" value={totals.turns} hint="skill uses across the window" />
        <StatTile
          label="Versions written"
          value={totals.versions}
          hint={`${totals.created} new, ${totals.retired} retired`}
        />
        <StatTile label="Idle" value={stats.idle.length} hint="carried, used by nothing" />
      </div>

      {stats.used.length > 0 && (
        <div className="rounded-lg border border-surface-200 bg-white p-4 dark:border-surface-800 dark:bg-surface-900">
          <p className="text-xs font-medium uppercase tracking-wide text-surface-500 dark:text-surface-400">
            Most used
          </p>
          <ul className="mt-3 space-y-2">
            {stats.used.map((s) => (
              <li key={s.skill_id} className="text-sm">
                <div className="flex items-baseline justify-between gap-3">
                  <span className="truncate text-surface-800 dark:text-surface-200">{s.name}</span>
                  <span
                    className="shrink-0 tabular-nums text-surface-500 dark:text-surface-400"
                    title={exact(s.turns)}
                  >
                    {compact(s.turns)} turns
                  </span>
                </div>
                {/* Against the busiest rather than the total: several skills
                    serve one turn between them, so shares of a total would all
                    read as tiny and say nothing about which is relied on. */}
                <div className="mt-1 h-1 rounded bg-surface-100 dark:bg-surface-800">
                  <div
                    className="h-1 rounded bg-brand-500"
                    style={{ width: `${busiest > 0 ? (s.turns / busiest) * 100 : 0}%` }}
                  />
                </div>
                <p className="mt-0.5 text-xs text-surface-500 dark:text-surface-400">
                  {s.sessions} {s.sessions === 1 ? 'conversation' : 'conversations'} ·{' '}
                  {since(s.last_used)}
                  {s.versions > 0 &&
                    ` · edited ${s.versions === 1 ? 'once' : `${s.versions} times`} in this window`}
                </p>
              </li>
            ))}
          </ul>
        </div>
      )}

      {stats.lagging.length > 0 && (
        <div className="rounded-lg border border-amber-300 bg-amber-50 p-4 dark:border-amber-900 dark:bg-amber-950/40">
          <p className="flex items-center gap-2 text-xs font-medium uppercase tracking-wide text-amber-800 dark:text-amber-300">
            <Clock className="h-3.5 w-3.5" aria-hidden />
            Running an older version
          </p>
          <ul className="mt-3 space-y-2 text-sm">
            {stats.lagging.map((s) => (
              <li key={s.skill_id}>
                <span className="text-surface-800 dark:text-surface-200">{s.name}</span>{' '}
                <span className="text-surface-600 dark:text-surface-400">
                  served v{s.serving} on {compact(s.turns)}{' '}
                  {s.turns === 1 ? 'turn' : 'turns'}, latest is v{s.latest}
                </span>
                {/* The whole point of the panel: a pin is somebody's decision,
                    its absence is an edit nobody picked up. */}
                <p className="text-xs text-surface-500 dark:text-surface-400">
                  {s.pinned
                    ? 'An agent pins this version, so the newer one is deliberate.'
                    : 'Nothing pins it — the newer version may not have reached the agent.'}
                </p>
              </li>
            ))}
          </ul>
        </div>
      )}

      {stats.idle.length > 0 && (
        <div className="rounded-lg border border-surface-200 bg-white p-4 dark:border-surface-800 dark:bg-surface-900">
          <p className="flex items-center gap-2 text-xs font-medium uppercase tracking-wide text-surface-500 dark:text-surface-400">
            <PauseCircle className="h-3.5 w-3.5" aria-hidden />
            Carried, and used by nothing
          </p>
          <p className="mt-1 text-xs text-surface-500 dark:text-surface-400">
            A skill's instructions are sent on every round of every turn the
            agent takes, whether it uses them or not.
          </p>
          <ul className="mt-3 space-y-1 text-sm">
            {stats.idle.map((s) => (
              <li key={s.skill_id} className="flex items-baseline justify-between gap-3">
                <span className="truncate text-surface-800 dark:text-surface-200">{s.name}</span>
                <span className="shrink-0 text-xs text-surface-500 dark:text-surface-400">
                  {s.agents} {s.agents === 1 ? 'agent' : 'agents'} · last used {since(s.last_used)}
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}

      {stats.authors.length > 0 && (
        <div className="rounded-lg border border-surface-200 bg-white p-4 dark:border-surface-800 dark:bg-surface-900">
          <p className="text-xs font-medium uppercase tracking-wide text-surface-500 dark:text-surface-400">
            Who wrote them
          </p>
          <ul className="mt-3 space-y-1 text-sm">
            {stats.authors.map((a) => (
              <li
                key={a.user_id ?? 'platform'}
                className="flex items-baseline justify-between gap-3"
              >
                <span className="truncate text-surface-800 dark:text-surface-200">
                  {/* A version written by an install or a seed carries no
                      author. Named as such rather than left blank. */}
                  {a.name ?? 'the platform'}
                </span>
                <span className="shrink-0 text-xs text-surface-500 dark:text-surface-400">
                  {a.versions} {a.versions === 1 ? 'version' : 'versions'} across {a.skills}{' '}
                  {a.skills === 1 ? 'skill' : 'skills'}
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}

      {totals.skills === 0 && (
        <div className="rounded-lg border border-dashed border-surface-300 p-6 text-center text-sm text-surface-500 dark:border-surface-700 dark:text-surface-400">
          <AlertTriangle className="mx-auto mb-2 h-4 w-4" aria-hidden />
          This workspace has no skills yet.
        </div>
      )}
    </section>
  )
}
