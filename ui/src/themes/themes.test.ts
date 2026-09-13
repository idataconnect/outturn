import { readFileSync, readdirSync, type Dirent } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'

const DIR = dirname(fileURLToPath(import.meta.url))
const themes = readdirSync(DIR).filter((f) => f.endsWith('.css'))
const read = (f: string) => readFileSync(join(DIR, f), 'utf8')

/** Every `--color-*` / `--font-*` a file defines. */
function tokensOf(css: string): Set<string> {
  return new Set(
    Array.from(css.matchAll(/^\s*(--(?:color|font)-[a-z0-9-]+):/gm)).map((m) => m[1]),
  )
}

const DEFAULT = 'outturn.css'

describe('themes', () => {
  it('ships more than one, so the tokens are tested by use', () => {
    expect(themes.length).toBeGreaterThan(1)
    expect(themes).toContain(DEFAULT)
  })

  // The default is the contract. A theme missing a token does not fail
  // loudly -- it silently inherits outturn's value and looks like outturn in
  // one corner, which is exactly the bug a second theme exists to catch.
  it.each(themes.filter((t) => t !== DEFAULT).map((t) => [t] as const))(
    '%s defines every token the default does',
    (theme) => {
      const expected = tokensOf(read(DEFAULT))
      const actual = tokensOf(read(theme))
      const missing = [...expected].filter((t) => !actual.has(t))
      expect(missing).toEqual([])
    },
  )

  it.each(themes.map((t) => [t] as const))('%s sets tokens and paints nothing itself', (theme) => {
    const css = read(theme)
    // A theme that writes rules is a theme that fights the components. Strip
    // the @theme block and any @import, and nothing with a selector is left.
    const withoutTheme = css.replace(/@theme\s*\{[\s\S]*?\n\}/g, '')
    const rules = withoutTheme.replace(/\/\*[\s\S]*?\*\//g, '').replace(/@import[^;]*;/g, '')
    expect(rules).not.toMatch(/[.#a-z][^{}]*\{/i)
  })
})

describe('the default look does not leak into components', () => {
  const src = join(dirname(fileURLToPath(import.meta.url)), '..')
  function walk(dir: string): string[] {
    return readdirSync(dir, { withFileTypes: true }).flatMap((e: Dirent) => {
      const p = join(dir, e.name)
      if (e.isDirectory()) return walk(p)
      return /\.tsx?$/.test(e.name) && !/\.test\./.test(e.name) ? [p] : []
    })
  }

  // Stock Tailwind hues are how outturn's own palette gets written into a
  // component by accident -- `to-orange-500` on the avatar did exactly that,
  // and put outturn's accent on every deployment's initials.
  //
  // Only the hues outturn's own identity is made of. Red, amber and green are
  // deliberately stock: a warning should read as a warning whoever is running
  // this, and tying status to a brand means a deployment whose brand is red
  // has no way left to say "destructive".
  const BRANDISH =
    /\b(?:bg|text|border|ring|from|to|via)-(?:teal|cyan|orange|sky|blue|indigo|violet|purple|fuchsia|pink)-\d{2,3}\b/

  it('no component reaches for a stock brand hue', () => {
    const offenders = walk(src)
      .flatMap((file) => {
        const hits = readFileSync(file, 'utf8').match(BRANDISH) ?? []
        return hits.map((h: string) => `${file.replace(src, 'src')}: ${h}`)
      })
    expect(offenders).toEqual([])
  })
})
