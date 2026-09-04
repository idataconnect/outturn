import { useState } from 'react'
import type { ToolCallMessagePartProps } from '@assistant-ui/react'
import { ChevronRight, TriangleAlert, Wrench } from 'lucide-react'

/**
 * A tool the agent ran, shown above the reply it fed.
 *
 * Rendered as the fallback for every tool rather than registering each one by
 * name: the shape is the same for all of them, and a tool that arrives without
 * a renderer should still be visible rather than silently missing.
 *
 * The action is the model's own label for what it was doing, and it leads,
 * because it is the part written for a person; the tool's name is the detail
 * underneath. Details are what the tool actually returned -- never sent to the
 * model, and shown only when asked for.
 *
 * A tool with nothing worth showing gets no expander at all. An affordance
 * that opens onto nothing teaches people to stop opening them.
 */
export default function ToolCall({ toolName, args }: ToolCallMessagePartProps) {
  const [open, setOpen] = useState(false)

  const action = typeof args?.action === 'string' ? args.action : ''
  const details = typeof args?.details === 'string' ? args.details : ''
  const isError = args?.isError === true

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
        </div>

        {details && (
          <button
            type="button"
            onClick={() => setOpen((shown) => !shown)}
            aria-expanded={open}
            className="flex shrink-0 items-center gap-0.5 rounded px-1 py-0.5 text-[11px] text-surface-500 hover:text-surface-800 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-500/40 dark:text-surface-400 dark:hover:text-surface-200"
          >
            <ChevronRight
              className={`h-3 w-3 transition-transform ${open ? 'rotate-90' : ''}`}
              aria-hidden
            />
            {open ? 'Hide' : 'Show'}
          </button>
        )}
      </div>

      {details && open && (
        <pre className="mt-2 max-h-64 overflow-auto rounded border border-surface-200 bg-white p-2 font-mono text-[11px] leading-relaxed text-surface-700 dark:border-surface-800 dark:bg-surface-900 dark:text-surface-300">
          {details}
        </pre>
      )}
    </div>
  )
}
