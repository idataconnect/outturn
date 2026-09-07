import { MarkdownTextPrimitive, unstable_memoizeMarkdownComponents } from '@assistant-ui/react-markdown'
import remarkBreaks from 'remark-breaks'
import remarkGfm from 'remark-gfm'

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

  pre: (props) => (
    <pre
      className="mb-2 overflow-x-auto rounded-md bg-surface-100 dark:bg-surface-950 group-[.user]:bg-black/25 p-3 text-xs"
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
  return <MarkdownTextPrimitive remarkPlugins={[remarkGfm]} components={components} />
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
