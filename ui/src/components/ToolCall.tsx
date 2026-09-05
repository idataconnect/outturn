import type { ToolCallMessagePartProps } from '@assistant-ui/react'
import { TriangleAlert, Wrench } from 'lucide-react'

/**
 * A tool the agent ran, shown above the reply it fed.
 *
 * The verb and nothing else. This is the fallback -- what every tool without a
 * renderer of its own gets -- and a fallback that shows a tool's output shows
 * whatever shape that output happens to have, which is JSON. An expander onto
 * raw JSON is not disclosure; it is the machinery showing through, and it
 * arrives by default on every tool anyone adds later.
 *
 * So showing more than the verb is opt-in, and opting in means writing a
 * renderer for that specific tool in `toolRenderers`. That forces the question
 * "what would a person want to see here" to be answered once per tool, by
 * someone who knows the answer, instead of being answered "the JSON" for all
 * of them by this file.
 *
 * Failures are the exception: something did not go as the verb said it would,
 * and the reader has no other way to find out what.
 */
export default function ToolCall({ toolName, args }: ToolCallMessagePartProps) {
  const action = typeof args?.action === 'string' ? args.action : ''
  const isError = args?.isError === true
  const details = typeof args?.details === 'string' ? args.details : ''

  return (
    <div
      className={`mb-2 rounded-md border px-3 py-2 text-xs ${
        isError
          ? 'border-red-300 bg-red-50 dark:border-red-900 dark:bg-red-950/40'
          : 'border-surface-200 bg-surface-50 dark:border-surface-800 dark:bg-surface-950'
      }`}
    >
      <div className="flex items-start gap-2">
        {isError ? (
          <TriangleAlert
            className="mt-0.5 h-3.5 w-3.5 shrink-0 text-red-600 dark:text-red-400"
            aria-hidden
          />
        ) : (
          <Wrench
            className="mt-0.5 h-3.5 w-3.5 shrink-0 text-brand-600 dark:text-brand-400"
            aria-hidden
          />
        )}

        <div className="min-w-0 flex-1">
          {action ? (
            <>
              <p className="text-surface-700 dark:text-surface-200">{action}</p>
              <span className="font-mono text-[11px] text-surface-500 dark:text-surface-500">
                {toolName}
              </span>
            </>
          ) : (
            <span className="font-mono text-surface-700 dark:text-surface-300">{toolName}</span>
          )}

          {isError && details && (
            <p className="mt-1 text-red-700 dark:text-red-300">{details}</p>
          )}
        </div>
      </div>
    </div>
  )
}
