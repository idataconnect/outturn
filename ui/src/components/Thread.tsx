import { ComposerPrimitive, MessagePrimitive, ThreadPrimitive } from '@assistant-ui/react'
import { Send } from 'lucide-react'

import MarkdownText from './MarkdownText'

/**
 * Thread built from assistant-ui primitives directly, using this project's
 * Tailwind classes. The prebuilt component is distributed through shadcn's
 * generator, which would pull in path aliases, components.json and a second
 * styling convention for no benefit here.
 */

function UserMessage() {
  return (
    <MessagePrimitive.Root className="flex justify-end">
      <div className="max-w-[75%] px-4 py-2 rounded-lg text-sm whitespace-pre-wrap bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white">
        <MessagePrimitive.Parts />
      </div>
    </MessagePrimitive.Root>
  )
}

function AssistantMessage() {
  return (
    <MessagePrimitive.Root className="flex justify-start">
      <div className="max-w-[75%] px-4 py-2 rounded-lg text-sm bg-white dark:bg-surface-900 border border-surface-200 dark:border-surface-800 text-surface-900 dark:text-surface-100">
        {/* Only the assistant's text is markdown: rendering what the user
            typed would mangle anything that resembled syntax. */}
        <MessagePrimitive.Parts components={{ Text: MarkdownText }} />
      </div>
    </MessagePrimitive.Root>
  )
}

export default function Thread({ disabled }: { disabled?: boolean }) {
  return (
    <ThreadPrimitive.Root className="flex flex-col h-full">
      <ThreadPrimitive.Viewport className="flex-1 overflow-auto p-6 space-y-4">
        <ThreadPrimitive.Empty>
          <p className="text-sm text-surface-600 dark:text-surface-400">
            {disabled ? 'Start a session to begin chatting.' : 'Send a message to begin.'}
          </p>
        </ThreadPrimitive.Empty>

        <ThreadPrimitive.Messages
          components={{
            UserMessage,
            AssistantMessage,
          }}
        />

        <ThreadPrimitive.If running>
          <p className="text-xs text-surface-600 dark:text-surface-400">Thinking…</p>
        </ThreadPrimitive.If>
      </ThreadPrimitive.Viewport>

      <ComposerPrimitive.Root className="flex gap-2 p-4 border-t border-surface-200 dark:border-surface-800">
        <ComposerPrimitive.Input
          autoFocus
          disabled={disabled}
          placeholder={disabled ? 'Start a session first' : 'Message the agent…'}
          className="flex-1 px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100 resize-none disabled:opacity-50"
        />
        <ComposerPrimitive.Send
          disabled={disabled}
          className="flex items-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium disabled:opacity-50"
        >
          <Send size={16} />
        </ComposerPrimitive.Send>
      </ComposerPrimitive.Root>
    </ThreadPrimitive.Root>
  )
}
