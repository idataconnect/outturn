/**
 * A line diff, for showing what one version of a skill changed.
 *
 * Its own small implementation rather than a dependency. The whole need is "show
 * somebody what is different before they press Restore", the inputs are two short
 * prose documents, and a diff library brings a matching algorithm, word-level
 * refinement and a rendering opinion for a page that wants none of them.
 *
 * Longest-common-subsequence, which is what `diff` and `git` use for the same job
 * and is O(n*m) in lines. A skill body is prose somebody wrote by hand, so n and m
 * are tens or hundreds; the guard below is for the pathological paste rather than
 * for anything a person types.
 */

/** How many lines either side may have before this gives up and says so. */
export const MAX_LINES = 2000

export type Change =
  | { kind: 'same'; text: string; line: number }
  | { kind: 'added'; text: string; line: number }
  | { kind: 'removed'; text: string; line: number }

/**
 * What changed between two documents, line by line.
 *
 * `line` numbers the side the line belongs to -- the new one for `same` and
 * `added`, the old one for `removed` -- so a reader can find it in the file it
 * came from rather than in a rendering of the diff.
 *
 * Returns `null` when either side is too long to diff, so the caller can say so
 * rather than freezing the page. Announcing the limit beats a tab that stops
 * responding, and a skill body that hits it is a skill nobody reads either.
 */
export function lineDiff(before: string, after: string): Change[] | null {
  const a = before.split('\n')
  const b = after.split('\n')
  if (a.length > MAX_LINES || b.length > MAX_LINES) return null

  // The LCS table, as lengths rather than as the subsequence itself: the walk
  // below reconstructs the path, so there is nothing to store per cell but a
  // count.
  const lcs: number[][] = Array.from({ length: a.length + 1 }, () =>
    new Array<number>(b.length + 1).fill(0),
  )
  for (let i = a.length - 1; i >= 0; i--) {
    for (let j = b.length - 1; j >= 0; j--) {
      lcs[i][j] = a[i] === b[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1])
    }
  }

  const out: Change[] = []
  let i = 0
  let j = 0
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      out.push({ kind: 'same', text: b[j], line: j + 1 })
      i++
      j++
    } else if (lcs[i + 1][j] >= lcs[i][j + 1]) {
      // Removals before additions where both are possible, so a changed line
      // reads as the old text struck out and the new text under it rather than
      // the other way round.
      out.push({ kind: 'removed', text: a[i], line: i + 1 })
      i++
    } else {
      out.push({ kind: 'added', text: b[j], line: j + 1 })
      j++
    }
  }
  while (i < a.length) {
    out.push({ kind: 'removed', text: a[i], line: i + 1 })
    i++
  }
  while (j < b.length) {
    out.push({ kind: 'added', text: b[j], line: j + 1 })
    j++
  }
  return out
}

/** Whether a diff has anything in it worth showing. */
export function hasChanges(changes: Change[]): boolean {
  return changes.some((c) => c.kind !== 'same')
}

/**
 * The diff with long runs of unchanged lines collapsed.
 *
 * A version that changed one sentence of a two-page body is the ordinary case, and
 * showing both pages to find it is what makes a diff useless. `context` lines
 * either side of every change are kept; the rest become a single gap the caller
 * renders as "N unchanged lines".
 */
export type Section = { kind: 'changes'; changes: Change[] } | { kind: 'gap'; lines: number }

export function collapse(changes: Change[], context = 3): Section[] {
  const keep = new Array<boolean>(changes.length).fill(false)
  changes.forEach((change, at) => {
    if (change.kind === 'same') return
    for (let i = Math.max(0, at - context); i <= Math.min(changes.length - 1, at + context); i++) {
      keep[i] = true
    }
  })

  const out: Section[] = []
  let run: Change[] = []
  let gap = 0
  const flushRun = () => {
    if (run.length) {
      out.push({ kind: 'changes', changes: run })
      run = []
    }
  }
  const flushGap = () => {
    if (gap) {
      out.push({ kind: 'gap', lines: gap })
      gap = 0
    }
  }
  changes.forEach((change, at) => {
    if (keep[at]) {
      flushGap()
      run.push(change)
    } else {
      flushRun()
      gap++
    }
  })
  flushRun()
  flushGap()
  return out
}
