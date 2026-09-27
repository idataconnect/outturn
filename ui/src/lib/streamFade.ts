/**
 * Fades streamed text in, a commit at a time.
 *
 * A rehype plugin rather than anything wrapped around deltas, because the
 * markdown is re-parsed on every commit and the tree is the only place the new
 * text can be found once it has been parsed. Each commit's text starts at a
 * source offset; text nodes are split at those offsets and every piece wrapped
 * in `<span class="f">`, which index.css animates.
 *
 * React is what makes this cheap. A piece already on screen keeps its index and
 * its content across re-parses, so it is kept rather than remounted and does
 * not fade again; only the pieces a commit added are new elements, and only
 * new elements run the animation. Where a re-parse changes structure -- `**`
 * closing turns text into `<strong>` -- the pieces under it remount and fade
 * once more, which reads as the emphasis arriving.
 *
 * Only text that was seen arriving is split. A message that was complete when
 * it first rendered records no offsets and is left exactly as parsed, so
 * opening a conversation does not fade its history in.
 *
 * The spans stay once the reply is complete. Taking them out would replace
 * every text node in the message at the moment somebody is reading it, and
 * would look the same.
 */

// The shape of the hast nodes this touches, rather than a dependency on the
// types package for four fields.
type Point = { offset?: number }
type Position = { start: Point; end: Point }
type Text = { type: 'text'; value: string; position?: Position }
type Element = {
  type: 'element'
  tagName: string
  properties: Record<string, unknown>
  children: Node[]
}
type Node = Text | Element | { type: string; children?: Node[] }
type File = { value: unknown }

/**
 * What one message's renderer remembers between parses. One per rendered
 * part: offsets are positions in that part's text and mean nothing elsewhere.
 */
export type FadeState = {
  /** Where each commit's text starts, ascending. */
  starts: number[]
  /** The text's length at the last parse; null before the first. */
  seen: number | null
}

export function newFadeState(): FadeState {
  return { starts: [], seen: null }
}

/**
 * `running` is whether the part is streaming, and matters only at the first
 * parse: it decides whether the text already there arrived or was always there.
 */
export function rehypeStreamFade(state: FadeState, running: boolean) {
  return () => (tree: Node, file: File) => {
    const length = typeof file.value === 'string' ? file.value.length : 0
    record(state, length, running)
    if (state.starts.length > 0) split(tree, state.starts)
  }
}

function record(state: FadeState, length: number, running: boolean) {
  if (state.seen === null) {
    // Mounted mid-stream: everything so far is still arriving. Mounted
    // complete: nothing is, and no offset is ever recorded.
    state.seen = running ? 0 : length
    if (!running) return
  }
  if (length < state.seen) {
    // The text went backwards -- regenerated, or edited. Offsets past the new
    // end describe text that no longer exists.
    state.starts = state.starts.filter((s) => s < length)
  } else if (length > state.seen) {
    state.starts.push(state.seen)
  }
  state.seen = length
}

function split(node: Node, starts: number[]) {
  if (!('children' in node) || !node.children) return
  node.children = node.children.flatMap((child) => {
    if (child.type === 'text') return pieces(child as Text, starts)
    split(child, starts)
    return [child]
  })
}

// A commit can end partway through a character as a reader sees one: between
// the two halves of a surrogate pair, or inside a flag or a joined emoji
// sequence. Cut there and each span holds a fragment -- a lone surrogate, or
// a regional indicator drawn as a letter -- which some browsers draw
// correctly by shaping across the boundary and others do not, and which is
// broken in the DOM either way. So a cut that is not between two graphemes is
// not made, and that piece joins the one before it.
const graphemes = new Intl.Segmenter(undefined, { granularity: 'grapheme' })

function isGraphemeBoundary(value: string, at: number): boolean {
  // Almost every cut is between two characters below the combining marks,
  // which cannot be part of one cluster -- except a CRLF. Asking the
  // segmenter walks the text from its start, and this runs for every cut on
  // every commit.
  const before = value.charCodeAt(at - 1)
  const after = value.charCodeAt(at)
  if (before < 0x300 && after < 0x300) return !(before === 0x0d && after === 0x0a)
  return graphemes.segment(value).containing(at)?.index === at
}

function pieces(text: Text, starts: number[]): Node[] {
  const from = text.position?.start.offset
  const to = text.position?.end.offset
  // Offsets only say where a character is when the node's text is the source
  // verbatim. An entity or an escape makes them differ, and a node like that
  // is left whole: it appears without fading, which is better than fading
  // the wrong characters.
  if (from === undefined || to === undefined || to - from !== text.value.length) {
    return [text]
  }
  const cuts = [
    from,
    ...starts.filter((s) => s > from && s < to && isGraphemeBoundary(text.value, s - from)),
    to,
  ]
  const out: Node[] = []
  for (let i = 0; i < cuts.length - 1; i++) {
    const value = text.value.slice(cuts[i] - from, cuts[i + 1] - from)
    // Text from before anything was seen arriving stays bare, as it was
    // before the first offset was recorded. Wrapping it now would make it a
    // new element, and it would fade in a second time.
    if (cuts[i] < starts[0]) {
      out.push({ type: 'text', value })
      continue
    }
    out.push({
      type: 'element',
      tagName: 'span',
      properties: { className: ['f'] },
      children: [{ type: 'text', value }],
    })
  }
  return out
}
