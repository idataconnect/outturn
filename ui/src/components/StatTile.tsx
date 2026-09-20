import { compact, exact } from '../lib/viz'

/**
 * One headline number.
 *
 * Label, value, and a sentence saying what the number is of. No delta: a delta
 * needs a comparable previous window, and the summary does not return one --
 * a tile that showed "+12%" computed against a window nobody asked for would be
 * the dashboard's least trustworthy pixel.
 */
export default function StatTile({
  label,
  value,
  hint,
}: {
  label: string
  value: number
  hint?: string
}) {
  return (
    <div className="rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 p-4">
      <p className="text-xs font-medium uppercase tracking-wide text-surface-500 dark:text-surface-400">
        {label}
      </p>
      {/* Proportional figures, not tabular: these are standalone display
          numbers, and tabular widths make a number like 121 look loose. */}
      <p
        className="mt-1 text-2xl font-semibold text-surface-900 dark:text-surface-100"
        title={exact(value)}
      >
        {compact(value)}
      </p>
      {hint && <p className="mt-0.5 text-xs text-surface-500 dark:text-surface-400">{hint}</p>}
    </div>
  )
}
