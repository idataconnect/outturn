import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import Markdown from 'react-markdown'
import { describe, expect, it } from 'vitest'
import { newFadeState, rehypeStreamFade, type FadeState } from './streamFade'

// Streaming unless a test says otherwise. Only the first render reads it.
function render(state: FadeState, text: string, running = true): string {
  return renderToStaticMarkup(
    createElement(Markdown, { rehypePlugins: [rehypeStreamFade(state, running)] }, text),
  )
}

const streaming = newFadeState

describe('rehypeStreamFade', () => {
  it('wraps each commit in its own span', () => {
    const state = streaming()
    render(state, 'Hello')
    expect(render(state, 'Hello there')).toBe(
      '<p><span class="f">Hello</span><span class="f"> there</span></p>',
    )
  })

  it('keeps earlier pieces identical as later ones arrive', () => {
    // What React relies on to leave them mounted, and so not fade them again.
    const state = streaming()
    render(state, 'One')
    const before = render(state, 'One two')
    const after = render(state, 'One two three')
    expect(after.startsWith(before.replace('</p>', ''))).toBe(true)
  })

  it('leaves a message that was complete when it first rendered alone', () => {
    const state = newFadeState()
    expect(render(state, 'Already here', false)).toBe('<p>Already here</p>')
  })

  it('fades only what arrives after a complete message grows', () => {
    const state = newFadeState()
    render(state, 'Already', false)
    expect(render(state, 'Already more')).toBe(
      '<p>Already<span class="f"> more</span></p>',
    )
  })

  it('splits across markup, piece by piece', () => {
    const state = streaming()
    render(state, 'Say **hi')
    expect(render(state, 'Say **hi** now')).toBe(
      '<p><span class="f">Say </span><strong><span class="f">hi</span></strong><span class="f"> now</span></p>',
    )
  })

  it('forgets offsets past the end when the text shrinks', () => {
    const state = streaming()
    render(state, 'abc')
    render(state, 'abcdef')
    render(state, 'ab')
    expect(state.starts).toEqual([0])
  })

  it('does not cut between the halves of a surrogate pair', () => {
    // Seen streaming "Item one 🥇": a commit ended after the high surrogate,
    // and the medal was split into two spans of one half each.
    const state = streaming()
    render(state, 'Item one \uD83E')
    expect(render(state, 'Item one 🥇')).toBe('<p><span class="f">Item one 🥇</span></p>')
  })

  it('does not cut inside a flag', () => {
    const state = streaming()
    render(state, 'Canada 🇨')
    render(state, 'Canada 🇨🇦')
    expect(render(state, 'Canada 🇨🇦 eh')).toBe(
      '<p><span class="f">Canada 🇨🇦</span><span class="f"> eh</span></p>',
    )
  })

  it('leaves a node whole when its text is not the source verbatim', () => {
    const state = streaming()
    render(state, 'a &amp;')
    expect(render(state, 'a &amp; b')).toBe('<p>a &amp; b</p>')
  })
})
