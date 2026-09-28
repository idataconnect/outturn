import type { ReasoningMessagePartProps } from '@assistant-ui/react'
import { useState } from 'react'
import { Brain, ChevronDown, ChevronRight } from 'lucide-react'

/**
 * A model's thinking, where it produced any.
 *
 * Collapsed by default, and deliberately: it is not the answer, and a reply
 * that opens with several paragraphs of deliberation buries the sentence the
 * reader came for. But it is not hidden either -- a turn spent entirely on
 * thinking used to render as an empty reply, so a reader could see that the
 * agent had answered nothing and had no way to find out why.
 *
 * Drawn as the agent's working rather than as something it said. The transcript
 * keeps it beside the reply for the same reason: thinking is never sent back to
 * a model as history, because a model handed its own reasoning as a past
 * utterance reads it as speech and answers it.
 */
export default function Reasoning({ text, status }: ReasoningMessagePartProps) {
  const [open, setOpen] = useState(false)
  // While it is still arriving there may be nothing else in the reply yet, so
  // the summary line is the only thing saying the turn is alive.
  const thinking = status?.type === 'running'

  if (text === '') return null

  return (
    <div className="mb-2 rounded-md border border-surface-200 bg-surface-50 px-3 py-2 text-xs dark:border-surface-700 dark:bg-surface-800">
      <button
        type="button"
        onClick={() => setOpen((was) => !was)}
        aria-expanded={open}
        className="flex w-full items-center gap-2 text-left text-surface-600 hover:text-surface-900 dark:text-surface-400 dark:hover:text-surface-100"
      >
        {open ? (
          <ChevronDown className="h-3.5 w-3.5 shrink-0" aria-hidden />
        ) : (
          <ChevronRight className="h-3.5 w-3.5 shrink-0" aria-hidden />
        )}
        <Brain
          className={`h-3.5 w-3.5 shrink-0 ${thinking ? 'animate-pulse text-brand-600 dark:text-brand-400' : ''}`}
          aria-hidden
        />
        <span>{thinking ? 'Thinking...' : 'Thought about this'}</span>
      </button>

      {open && (
        // Pre-wrapped rather than rendered as markdown. Thinking is a model
        // talking to itself: it is often half-formed, and formatting it as
        // prose presents a draft as though it were written for the reader.
        <p className="mt-2 whitespace-pre-wrap break-words font-mono text-[11px] leading-relaxed text-surface-600 dark:text-surface-400">
          {text}
        </p>
      )}
    </div>
  )
}
