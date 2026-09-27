import { describe, expect, it } from 'vitest'

import { collapse, hasChanges, lineDiff, MAX_LINES } from './lineDiff'

const kinds = (before: string, after: string) =>
  lineDiff(before, after)?.map((c) => `${c.kind[0]}${c.text}`)

describe('what changed between two versions', () => {
  it('says nothing changed when nothing did', () => {
    const changes = lineDiff('one\ntwo', 'one\ntwo')!
    expect(changes.every((c) => c.kind === 'same')).toBe(true)
    expect(hasChanges(changes)).toBe(false)
  })

  it('finds a line that was added', () => {
    expect(kinds('one\nthree', 'one\ntwo\nthree')).toEqual(['sone', 'atwo', 'sthree'])
  })

  it('finds a line that was removed', () => {
    expect(kinds('one\ntwo\nthree', 'one\nthree')).toEqual(['sone', 'rtwo', 'sthree'])
  })

  it('shows a changed line as the old text then the new', () => {
    // Removals before additions where both are possible, so a reader sees what
    // it was and then what it became rather than the reverse.
    expect(kinds('one\nold\nthree', 'one\nnew\nthree')).toEqual([
      'sone',
      'rold',
      'anew',
      'sthree',
    ])
  })

  it('numbers each line against the side it came from', () => {
    // So a reader can find it in the file rather than in the diff.
    const changes = lineDiff('a\ngone\nb', 'a\nb\nnew')!
    const removed = changes.find((c) => c.kind === 'removed')!
    const added = changes.find((c) => c.kind === 'added')!
    expect(removed.line).toBe(2) // the old file's second line
    expect(added.line).toBe(3) // the new file's third
  })

  it('handles one side being empty', () => {
    expect(kinds('', 'one')).toEqual(['r', 'aone'])
    expect(kinds('one', '')).toEqual(['rone', 'a'])
  })

  it('refuses a document too long to diff rather than freezing', () => {
    // Announcing the limit beats a tab that stops responding, and a body this
    // long is one nobody reads either.
    const huge = Array.from({ length: MAX_LINES + 1 }, (_, i) => `line ${i}`).join('\n')
    expect(lineDiff(huge, 'one')).toBeNull()
    expect(lineDiff('one', huge)).toBeNull()
  })

  it('does not refuse a document exactly at the limit', () => {
    const atLimit = Array.from({ length: MAX_LINES }, (_, i) => `line ${i}`).join('\n')
    expect(lineDiff(atLimit, atLimit)).not.toBeNull()
  })
})

describe('collapsing the unchanged middle', () => {
  const body = (n: number) => Array.from({ length: n }, (_, i) => `line ${i}`).join('\n')

  it('keeps context either side of a change and gaps the rest', () => {
    // A version that changed one sentence of two pages is the ordinary case, and
    // showing both pages to find it is what makes a diff useless.
    const before = body(40)
    const after = before.replace('line 20', 'line 20 changed')
    const sections = collapse(lineDiff(before, after)!, 2)

    expect(sections[0]).toEqual({ kind: 'gap', lines: 18 })
    const shown = sections.filter((s) => s.kind === 'changes')
    expect(shown).toHaveLength(1)
    const texts = shown[0].kind === 'changes' ? shown[0].changes.map((c) => c.text) : []
    expect(texts).toContain('line 20')
    expect(texts).toContain('line 20 changed')
    expect(texts).toContain('line 18')
    expect(texts).not.toContain('line 17')
  })

  it('gaps nothing when everything is close to a change', () => {
    const sections = collapse(lineDiff('a\nb', 'a\nc')!, 3)
    expect(sections.every((s) => s.kind === 'changes')).toBe(true)
  })

  it('is one gap when nothing changed at all', () => {
    const sections = collapse(lineDiff(body(20), body(20))!, 3)
    expect(sections).toEqual([{ kind: 'gap', lines: 20 }])
  })

  it('loses no line between the gaps and the runs', () => {
    // A collapsed diff that dropped a line would be a diff that lied.
    const before = body(60)
    const after = before.replace('line 10', 'ten').replace('line 50', 'fifty')
    const changes = lineDiff(before, after)!
    const sections = collapse(changes, 3)
    const counted = sections.reduce(
      (total, s) => total + (s.kind === 'gap' ? s.lines : s.changes.length),
      0,
    )
    expect(counted).toBe(changes.length)
  })
})
