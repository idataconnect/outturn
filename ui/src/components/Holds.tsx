import { OctagonX, PauseCircle, X } from 'lucide-react'

import type { Inhibitor } from '../lib/inhibitors'

/**
 * What is holding some work, and the way to lift it.
 *
 * Every hold is listed rather than only the strongest. A workspace both stopped
 * by a spend cap and waiting on somebody's approval would otherwise report only
 * the stop, hiding a request that is still expected to be answered -- see
 * `docs/inhibitors.md`.
 */
export default function Holds({
  held,
  canRelease,
  onRelease,
  /** Shown when nothing is held. Omitted where silence is the better answer. */
  quiet,
}: {
  held: Inhibitor[]
  canRelease: (hold: Inhibitor) => boolean
  onRelease: (hold: Inhibitor) => void
  quiet?: string
}) {
  if (held.length === 0) {
    return quiet ? (
      <p className="text-sm text-surface-600 dark:text-surface-400">{quiet}</p>
    ) : null
  }

  return (
    <ul className="space-y-2">
      {held.map((hold) => {
        const stopped = hold.strength === 'stopped'
        const Icon = stopped ? OctagonX : PauseCircle
        return (
          <li
            key={hold.id}
            className={`flex items-start gap-3 p-3 rounded-md border ${
              stopped
                ? 'border-red-300 bg-red-50 dark:border-red-900 dark:bg-red-950/40'
                : 'border-amber-300 bg-amber-50 dark:border-amber-900 dark:bg-amber-950/40'
            }`}
          >
            <Icon
              size={16}
              aria-hidden
              className={`mt-0.5 shrink-0 ${
                stopped
                  ? 'text-red-600 dark:text-red-400'
                  : 'text-amber-600 dark:text-amber-400'
              }`}
            />
            <div className="flex-1 min-w-0">
              <p className="text-sm text-surface-900 dark:text-surface-100">
                {/* The scope, because a hold on the whole workspace and one on
                    a single agent look identical otherwise, and only one of
                    them explains why everything else is quiet too. */}
                <span className="font-medium">{describe(hold)}</span>
                {' — '}
                {hold.reason}
              </p>
              <p className="mt-0.5 text-xs text-surface-600 dark:text-surface-400">
                Held by {hold.held_by} since{' '}
                <time dateTime={hold.created_at}>
                  {new Date(hold.created_at).toLocaleString()}
                </time>
              </p>
            </div>
            {canRelease(hold) && (
              <button
                type="button"
                onClick={() => onRelease(hold)}
                aria-label={`Release: ${hold.reason}`}
                title="Release this hold"
                className="p-1.5 rounded-md text-surface-500 hover:text-surface-900 dark:hover:text-surface-100 hover:bg-surface-200/60 dark:hover:bg-surface-800"
              >
                <X size={14} aria-hidden />
              </button>
            )}
          </li>
        )
      })}
    </ul>
  )
}

/** What this hold covers, in words rather than a level name. */
function describe(hold: Inhibitor): string {
  const what =
    hold.scope.level === 'platform'
      ? 'Everything on this platform'
      : hold.scope.level === 'workspace'
        ? 'This whole workspace'
        : hold.scope.level === 'agent'
          ? 'This agent'
          : 'One conversation'
  return hold.strength === 'stopped' ? `${what}: stopped` : `${what}: waiting`
}
