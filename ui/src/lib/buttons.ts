/**
 * How an icon button looks.
 *
 * These had drifted into four near-identical strings: some rounded, some
 * rounded-md, some lighting up on hover and some only changing colour. The
 * ones that only changed colour read as inert next to the ones that did not,
 * because a 14px glyph going one shade darker is not much of an answer to
 * being pointed at.
 *
 * One background, one radius, everywhere -- so a close button feels like the
 * collapse button beside it.
 */

/** A quiet icon button: toolbar actions, close and collapse controls. */
export const iconButton =
  'p-1 rounded-md text-surface-400 hover:text-surface-900 dark:hover:text-surface-100 ' +
  'hover:bg-surface-100 dark:hover:bg-surface-800 transition-colors'

/** The same, at the larger tap target a header or a rail wants. */
export const iconButtonLarge =
  'p-1.5 rounded-md text-surface-500 hover:text-surface-900 dark:hover:text-surface-100 ' +
  'hover:bg-surface-100 dark:hover:bg-surface-800 transition-colors'

/**
 * Destructive: the hover colour carries the warning, so it keeps its red
 * rather than going the way of the others, but it gains the same background
 * so it sits level with its neighbours.
 */
export const iconButtonDanger =
  'p-1 rounded-md text-surface-400 hover:text-red-600 dark:hover:text-red-400 ' +
  'hover:bg-red-50 dark:hover:bg-red-950/40 transition-colors'
