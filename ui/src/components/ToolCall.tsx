import type { ToolCallMessagePartProps } from '@assistant-ui/react'
import { Wrench } from 'lucide-react'

/**
 * A tool the agent ran, shown above the reply it fed.
 *
 * Rendered as the fallback for every tool rather than registering each one by
 * name: the shape is the same for all of them, and a tool that arrives without
 * a renderer should still be visible rather than silently missing.
 *
 * The action is the model's own label for what it was doing, asked for as an
 * argument on every tool. It leads, because it is the part written for a
 * person; the tool's name is the detail underneath. It is never sent back to
 * the model, so this is the only place it is ever read.
 */
export default function ToolCall({ toolName, args }: ToolCallMessagePartProps) {
  const action = typeof args?.action === 'string' ? args.action : ''

  return (
    <div className="mb-2 flex items-start gap-2 rounded-md border border-surface-200 bg-surface-50 px-3 py-2 text-xs dark:border-surface-800 dark:bg-surface-950">
      <Wrench
        className="mt-0.5 h-3.5 w-3.5 shrink-0 text-brand-600 dark:text-brand-400"
        aria-hidden
      />
      <div className="min-w-0">
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
    </div>
  )
}
