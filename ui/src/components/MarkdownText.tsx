import { useAuiState } from '@assistant-ui/react'
import { MarkdownTextPrimitive, unstable_memoizeMarkdownComponents } from '@assistant-ui/react-markdown'
import { useMemo, useState } from 'react'
import Markdown from 'react-markdown'
import remarkBreaks from 'remark-breaks'
import remarkGfm from 'remark-gfm'
import { newFadeState, rehypeStreamFade } from '../lib/streamFade'

/**
 * Renders message text as markdown, for both sides of the conversation.
 *
 * Components are memoized because this re-renders on every delta while a reply
 * streams: without it each token would re-parse and re-render the whole
 * message, which gets expensive on a long answer.
 *
 * The same components serve the user's bubble, which sits on a brand-coloured
 * background rather than the surface. Anything that paints its own background
 * or border carries a `group-[.user]:` variant so it stays legible there; the
 * bubble marks itself with `group user`.
 */
const components = unstable_memoizeMarkdownComponents({
  h1: (props) => (
    <h1 className="mb-2 mt-4 text-xl font-semibold first:mt-0" {...props} />
  ),
  h2: (props) => (
    <h2 className="mb-2 mt-4 text-lg font-semibold first:mt-0" {...props} />
  ),
  h3: (props) => (
    <h3 className="mb-1 mt-3 font-semibold first:mt-0" {...props} />
  ),
  p: (props) => <p className="mb-2 last:mb-0 leading-relaxed" {...props} />,
  a: (props) => (
    <a
      className="underline underline-offset-2 hover:no-underline"
      target="_blank"
      rel="noreferrer"
      {...props}
    />
  ),
  ul: (props) => <ul className="mb-2 ml-5 list-disc space-y-1" {...props} />,
  ol: (props) => <ol className="mb-2 ml-5 list-decimal space-y-1" {...props} />,
  blockquote: (props) => (
    <blockquote
      className="mb-2 border-l-2 border-surface-300 dark:border-surface-700 group-[.user]:border-white/40 pl-3 italic"
      {...props}
    />
  ),

  // Wide tables scroll within the message rather than stretching the thread.
  table: (props) => (
    <div className="mb-2 overflow-x-auto">
      <table className="w-full border-collapse text-sm" {...props} />
    </div>
  ),
  th: (props) => (
    <th
      className="border border-surface-300 dark:border-surface-700 group-[.user]:border-white/30 px-2 py-1 text-left font-semibold bg-surface-100 dark:bg-surface-800 group-[.user]:bg-white/10"
      {...props}
    />
  ),
  td: (props) => (
    <td className="border border-surface-300 dark:border-surface-700 group-[.user]:border-white/30 px-2 py-1" {...props} />
  ),

  // The class is merged rather than set before the spread: assistant-ui hands
  // a fenced block's <pre> a className of its own, which would replace ours
  // and take `overflow-x-auto` with it -- a long line then widened the thread.
  pre: ({ className, ...props }) => (
    <pre
      className={`mb-2 overflow-x-auto rounded-md bg-surface-100 dark:bg-surface-800 group-[.user]:bg-black/25 p-3 text-xs ${className ?? ''}`}
      {...props}
    />
  ),
  code: ({ className, ...props }) => {
    // Inside a fence the <pre> already carries the styling; only inline code
    // needs its own background.
    const inline = !className?.includes('language-')
    return (
      <code
        className={
          inline
            ? 'rounded bg-surface-100 dark:bg-surface-800 group-[.user]:bg-black/25 px-1 py-0.5 font-mono text-[0.9em]'
            : 'font-mono'
        }
        {...props}
      />
    )
  },
  hr: () => <hr className="my-4 border-surface-200 dark:border-surface-800 group-[.user]:border-white/30" />,
})

export default function MarkdownText() {
  // One per part, for the part's life: it is what remembers which text has
  // already faded in. See lib/streamFade.ts. Mutable on purpose -- the plugin
  // writes to it as it parses, and nothing re-renders because of it.
  const [fade] = useState(newFadeState)
  const running = useAuiState((s) => s.part.status.type === 'running')
  const rehypePlugins = useMemo(() => [rehypeStreamFade(fade, running)], [fade, running])

  return (
    <MarkdownTextPrimitive
      remarkPlugins={[remarkGfm]}
      rehypePlugins={rehypePlugins}
      components={components}
      // Smoothing is on by default; these are its knobs. A local model
      // arrives in bursts -- a whole sentence, then nothing -- and revealing
      // at a steady rate reads as writing rather than as stuttering.
      //
      // Nothing about the DOM changes: the reveal feeds the same renderer a
      // shorter prefix for a moment, so a streaming message and a settled one
      // are the same markdown by the same path. That is the property worth
      // keeping -- a separate streaming representation is where the drift
      // bugs live.
      smooth={{
        // Longer than the 250ms default, so a burst is visibly drawn out
        // rather than landing at once.
        drainMs: 400,
        // Markdown is re-parsed on every commit, so committing every frame
        // means re-parsing a growing document 60 times a second. Once every
        // 16ms is one frame's worth and keeps the parse off the critical
        // path.
        minCommitMs: 16,
      }}
    />
  )
}

/**
 * The user's text, as markdown.
 *
 * With one difference from the assistant's: a single newline is a line break.
 * Someone typing a message presses Enter meaning "new line", not "same
 * paragraph", and standard markdown would join those lines back together.
 */
export function UserMarkdownText() {
  return (
    <MarkdownTextPrimitive remarkPlugins={[remarkGfm, remarkBreaks]} components={components} />
  )
}

/**
 * A model's thinking, as markdown.
 *
 * Takes its text as a prop rather than from the part context, so the thought
 * renders the same whether or not it sits inside a thread. Not smoothed: it is
 * collapsed while it streams, and opening it should show what has arrived.
 */
export function ThoughtMarkdownText({ text }: { text: string }) {
  return (
    <Markdown remarkPlugins={[remarkGfm]} components={components}>
      {text}
    </Markdown>
  )
}
