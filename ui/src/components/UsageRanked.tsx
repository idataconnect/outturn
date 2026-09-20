import { compact, exact, seriesColor, share } from '../lib/viz'
import { useDarkMode } from '../lib/useDarkMode'

/** One cut of the window, as the summary returns it. */
export type Slice = {
  key: string | null
  label: string | null
  calls: number
  tokens: number
}

/**
 * A dimension of the window, ranked.
 *
 * Horizontal rather than vertical because the categories are names -- models,
 * accounts, workspaces -- and a name under a column is either rotated or
 * truncated, both of which cost more than the extra height does.
 *
 * The bars are nominal, not ordinal: a model is not more of anything than
 * another model, so every bar takes the same slot-1 hue rather than being
 * coloured by its own value. Colouring nominal bars by rank spends the identity
 * channel re-encoding what the bar's length already says.
 */
export default function UsageRanked({
  slices,
  empty,
  unattributed = 'Not attributed',
  unit = 'tokens',
}: {
  slices: Slice[]
  /** What to say when the window holds nothing. */
  empty: string
  /**
   * What to call the row whose key is null.
   *
   * Each dimension's null means something different, and "Not attributed"
   * read as a gap in every one of them -- next to an agent list it looked
   * like spend that had gone astray, when it was the platform naming a
   * session and had nowhere else to go. A panel that knows why its column is
   * null says so here, and the reader learns what the row is rather than only
   * that something is missing.
   */
  unattributed?: string
  unit?: 'tokens' | 'calls'
}) {
  const dark = useDarkMode()
  const color = seriesColor(0, dark)

  const value = (s: Slice) => (unit === 'calls' ? s.calls : s.tokens)
  const total = slices.reduce((sum, s) => sum + value(s), 0)
  const max = Math.max(...slices.map(value), 1)

  if (slices.length === 0 || total === 0) {
    return <p className="text-sm text-surface-500 dark:text-surface-400">{empty}</p>
  }

  return (
    <ul className="space-y-2.5">
      {slices.map((slice) => {
        // A key with no label is already a name -- a model, an account. A key
        // with one is an id, and the name is what a reader knows it by.
        const name = slice.label ?? slice.key
        const v = value(slice)
        return (
          <li key={slice.key ?? 'unattributed'}>
            <div className="flex items-baseline justify-between gap-3 text-sm">
              <span
                className={`truncate ${
                  name
                    ? 'text-surface-800 dark:text-surface-200'
                    : 'text-surface-500 dark:text-surface-400 italic'
                }`}
                title={name ?? undefined}
              >
                {/* A null key is a row the ledger genuinely has no value for.
                    Named by the panel, which knows what its own null means;
                    inventing "Unknown" as though it were a category is not the
                    honest rendering, but neither is saying "missing" about a
                    column that was never going to be filled. */}
                {name ?? unattributed}
              </span>
              <span
                className="shrink-0 text-surface-900 dark:text-surface-100 font-medium"
                style={{ fontVariantNumeric: 'tabular-nums' }}
                title={`${exact(v)} ${unit}`}
              >
                {compact(v)}
                <span className="ml-1.5 text-xs font-normal text-surface-500 dark:text-surface-400">
                  {share(v, total)}
                </span>
              </span>
            </div>
            {/* The track is a lighter step of the same surface rather than a
                second hue, so the bar is the only thing carrying data ink. */}
            <div className="mt-1 h-2 rounded-sm bg-surface-100 dark:bg-surface-800 overflow-hidden">
              <div
                className="h-full rounded-sm"
                style={{ width: `${Math.max((v / max) * 100, 1.5)}%`, background: color }}
              />
            </div>
          </li>
        )
      })}
    </ul>
  )
}
