import type { ToolCallMessagePartProps } from '@assistant-ui/react'
import { Wrench } from 'lucide-react'

/**
 * A tool the agent ran, shown above the reply it fed.
 *
 * Rendered as the fallback for every tool rather than registering each one by
 * name: the shape is the same for all of them, and a tool that arrives without
 * a renderer should still be visible rather than silently missing.
 *
 * The reason is the model's own, asked for as an argument on every tool. It is
 * never sent back to the model, so this is the only place it is ever read.
 */
export default function ToolCall({ toolName, args }: ToolCallMessagePartProps) {
  const reason = typeof args?.reason === 'string' ? args.reason : ''

  return (
    <div className="mb-2 flex items-start gap-2 rounded-md border border-surface-200 bg-surface-50 px-3 py-2 text-xs dark:border-surface-800 dark:bg-surface-950">
      <Wrench
        className="mt-0.5 h-3.5 w-3.5 shrink-0 text-brand-600 dark:text-brand-400"
        aria-hidden
      />
      <div className="min-w-0">
        <span className="font-mono text-surface-700 dark:text-surface-300">{toolName}</span>
        {reason && (
          <p className="mt-0.5 text-surface-600 dark:text-surface-400">{reason}</p>
        )}
      </div>
    </div>
  )
}
