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
 * There may be several in one reply, each where the model stopped to think: a
 * thought before a tool call is about what to ask, and the one after it is
 * about the answer. Collecting them into a single block would say the model
 * deliberated once, about a result it had not yet seen.
 *
 * Drawn as the agent's working rather than as something it said. Thinking is
 * never sent back to a model as history, because a model handed its own
 * reasoning as a past utterance reads it as speech and answers it.
 */
/** What a thought cost, in the units we can honestly report.
 *
 * Words rather than tokens. Nothing in this codebase counts tokens -- a
 * per-model tokeniser is wrong for every model it was not built for -- and a
 * figure labelled "tokens" that was really a guess would be read as the number
 * on somebody's bill. Where a provider reports reasoning tokens of its own
 * they belong here instead, under their own name; until then this is the true
 * thing that can be said.
 *
 * Rounded to a tenth of a second under ten, then to whole seconds. A thought
 * that took 4.2s and one that took 4.23s are the same fact to a reader.
 */
function cost(text: string, ms: number | undefined): string {
  const words = text.trim() === '' ? 0 : text.trim().split(/\s+/).length
  const counted = `${words.toLocaleString()} ${words === 1 ? 'word' : 'words'}`
  if (ms === undefined) return counted
  const seconds = ms / 1000
  const shown = seconds < 10 ? seconds.toFixed(1) : Math.round(seconds).toString()
  return `${shown}s \u00b7 ${counted}`
}

export default function Reasoning({ text, status, ...part }: ReasoningMessagePartProps) {
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
        <span>
          {thinking ? (
            'Thinking...'
          ) : (
            <>
              Thought
              {/* Dimmer than the verb: the numbers are there for somebody
                  wondering where a turn went, not for everybody reading a
                  reply. */}
              <span className="ml-1.5 text-surface-500 dark:text-surface-500">
                {cost(text, (part as { ms?: number }).ms)}
              </span>
            </>
          )}
        </span>
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
