/**
 * The vocabulary every chart in this app draws with.
 *
 * Two things live here and nothing else: the series colours, and the small
 * formatting rules that make numbers comparable across panels. Components
 * import roles from here rather than writing hex or `toLocaleString` calls of
 * their own, so a deployment that replaces the theme replaces the charts with
 * it and a figure means the same thing wherever it appears.
 */

/**
 * The categorical slots, in fixed order.
 *
 * Derived from the theme's own ramps -- slot 1 is the brand teal that carries
 * everything interactive, slot 2 the accent the mark reserves -- and then
 * stepped until a validator passed them. Both modes clear the lightness band,
 * the chroma floor, adjacent-pair separation under protanopia and deuteranopia,
 * the normal-vision floor and 3:1 against their surface.
 *
 * The order is the safety mechanism, so slots are assigned in sequence and
 * never cycled. There are five, and nothing draws more: a ranked panel paints
 * every bar in slot 1, and the stacked chart has four bands plus the cache-read
 * line. Anything past the last slot takes the neutral below rather than a hue
 * nobody checked.
 *
 * Written as hex rather than as `var(--color-brand-600)` because these are not
 * the interface's colours: a chart needs steps chosen against the chart
 * surface, and reusing the interactive ramp would put an unvalidated pair side
 * by side the first time a workspace had two models.
 */
export const SERIES_LIGHT = ['#008e89', '#d55c13', '#6359b5', '#708500', '#b25196'] as const
export const SERIES_DARK = ['#04a19b', '#d8662a', '#7970d5', '#889e2a', '#c361a5'] as const

/** The colour of slot `i`, folding anything past the last slot onto a neutral. */
export function seriesColor(index: number, dark: boolean): string {
  const slots = dark ? SERIES_DARK : SERIES_LIGHT
  // Past the ceiling is the "others" fold, which is not an identity and should
  // not wear one -- a grey says "the rest" in a way a sixth hue cannot.
  return slots[index] ?? (dark ? '#8a8a80' : '#77776e')
}

/**
 * A count, shortened once it stops being readable in full.
 *
 * Tokens run to millions, and a dashboard that prints every digit of them makes
 * a reader count characters to compare two panels. Full precision stays
 * available on hover and in the table, so nothing is lost by rounding here.
 */
export function compact(n: number): string {
  const abs = Math.abs(n)
  if (abs >= 1_000_000_000) return `${trim(n / 1_000_000_000)}B`
  if (abs >= 1_000_000) return `${trim(n / 1_000_000)}M`
  if (abs >= 10_000) return `${trim(n / 1_000)}K`
  return n.toLocaleString()
}

/** One decimal, but only where it says something. */
function trim(n: number): string {
  const one = n.toFixed(1)
  return one.endsWith('.0') ? one.slice(0, -2) : one
}

/** The exact figure, for a tooltip or a table cell. */
export function exact(n: number): string {
  return n.toLocaleString()
}

/** A share of a total, guarding the empty window that would divide by zero. */
export function share(part: number, total: number): string {
  if (total <= 0) return '0%'
  const pct = (part / total) * 100
  return pct >= 10 || pct === 0 ? `${Math.round(pct)}%` : `${pct.toFixed(1)}%`
}

/**
 * The day a bucket opens, as a chart axis says it.
 *
 * The summary's buckets are midnight UTC, and they are rendered in UTC rather
 * than in the reader's zone on purpose: the window's bounds, the ledger's rows
 * and the axis all have to agree about which day a turn fell on, and a reader
 * in Auckland converting them locally would see a chart whose first and last
 * buckets are half empty for no visible reason.
 */
export function dayLabel(iso: string): string {
  return new Date(iso).toLocaleDateString(undefined, {
    month: 'short',
    day: 'numeric',
    timeZone: 'UTC',
  })
}

/** The same day, spelled out for a tooltip where there is room. */
export function dayLabelLong(iso: string): string {
  return new Date(iso).toLocaleDateString(undefined, {
    weekday: 'short',
    month: 'short',
    day: 'numeric',
    timeZone: 'UTC',
  })
}

/**
 * Round a maximum up to a number an axis can label.
 *
 * An axis topped at the data's own maximum prints ticks like 41,337, which
 * nobody reads; rounding to 1, 2 or 5 times a power of ten gives ticks that
 * divide evenly and a little headroom above the tallest mark.
 */
export function niceMax(value: number): number {
  if (value <= 0) return 1
  const magnitude = 10 ** Math.floor(Math.log10(value))
  for (const step of [1, 2, 2.5, 5, 10]) {
    const candidate = step * magnitude
    if (candidate >= value) return candidate
  }
  return 10 * magnitude
}

/**
 * Which `usage_source` values mean "nobody measured this".
 *
 * The ledger's column is a closed vocabulary -- `reported`, `reported_partial`,
 * `estimated`, `unknown` -- constrained in the schema and enumerated in Rust.
 * This is a fourth place it is written down, which is exactly the hazard
 * AGENTS.md describes: a fifth value added to the constraint would be counted
 * here as measured by default, silently, which is the one thing the column
 * exists to prevent.
 *
 * So the list is inverted: everything is unmeasured unless it is named as
 * measured. A value nobody has taught this page about then shows up in the
 * caveat rather than disappearing into the total -- loud and wrong rather than
 * quiet and wrong, which is the right way round for a figure people bill from.
 *
 * `reported_partial` counts as measured deliberately: the provider did report
 * it, and the numbers are real for the part that ran. It is a truncated turn,
 * not an unmeasured one.
 */
const MEASURED = new Set(['reported', 'reported_partial'])

/** Whether a `usage_source` means the figure was actually measured. */
export function measured(source: string | null): boolean {
  return source !== null && MEASURED.has(source)
}

/**
 * The kinds that stack, in order.
 *
 * Cache reads are deliberately *not* among them, and the omission is the whole
 * design of this chart. On a real transcript-heavy workload they dwarf
 * everything else -- a dev window here ran 874k cache reads against 86k prompt
 * and 30k completion -- so stacking them gives the band nine tenths of the
 * plot and squeezes completion tokens into five pixels. The reader loses the
 * series that actually move.
 *
 * Rescaling would not fix it honestly: a log axis makes a tenfold difference
 * look like a small one, and a second y-axis is never the answer. But the
 * separation is real rather than cosmetic -- a cache read is context being
 * re-read, priced differently by every provider and not new work the way a
 * prompt or a completion is. So the stack carries the work, and cache reads
 * ride above it as their own line against their own maximum.
 *
 * Prompt is the floor because it is the bulk and the least interesting: the
 * smaller bands sit where their movement is visible, against a flat base
 * rather than riding on one that moves under them.
 */
/** One day, already summed by the API. */
export type Bucket = {
  at: string
  calls: number
  prompt_tokens: number
  completion_tokens: number
  cache_read_tokens: number
  cache_write_tokens: number
  reasoning_tokens: number
}

export const KINDS = [
  { key: 'prompt_tokens', label: 'Prompt' },
  { key: 'completion_tokens', label: 'Completion' },
  { key: 'cache_write_tokens', label: 'Cache write' },
  { key: 'reasoning_tokens', label: 'Reasoning' },
] as const

/** Shown beside the stack rather than in it. See [`KINDS`]. */
export const CACHE_READ = { key: 'cache_read_tokens', label: 'Cache read' } as const

/**
 * Every token kind, stack and cache read together.
 *
 * The one list anything covering *all* of them reads -- the page's total, and
 * the table that stands in for this chart. They were written out separately
 * once and the table lost a column: it claimed to carry every figure the chart
 * encodes while silently omitting cache writes, which is the failure AGENTS.md
 * describes under "a job state is enumerated in more places than the schema".
 * A sixth kind now reaches all three by being added here.
 */
export const TOKEN_KINDS = [...KINDS, CACHE_READ] as const
