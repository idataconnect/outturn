import { ComposerPrimitive, MessagePrimitive, ThreadPrimitive } from '@assistant-ui/react'
import { Send } from 'lucide-react'

/**
 * Thread built from assistant-ui primitives directly, using this project's
 * Tailwind classes. The prebuilt component is distributed through shadcn's
 * generator, which would pull in path aliases, components.json and a second
 * styling convention for no benefit here.
 */

function UserMessage() {
  return (
    <MessagePrimitive.Root className="flex justify-end">
      <div className="max-w-[75%] px-4 py-2 rounded-lg text-sm whitespace-pre-wrap bg-gray-900 dark:bg-gray-100 text-white dark:text-gray-900">
        <MessagePrimitive.Parts />
      </div>
    </MessagePrimitive.Root>
  )
}

function AssistantMessage() {
  return (
    <MessagePrimitive.Root className="flex justify-start">
      <div className="max-w-[75%] px-4 py-2 rounded-lg text-sm whitespace-pre-wrap bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-800 text-gray-900 dark:text-gray-100">
        <MessagePrimitive.Parts />
      </div>
    </MessagePrimitive.Root>
  )
}

export default function Thread({ disabled }: { disabled?: boolean }) {
  return (
    <ThreadPrimitive.Root className="flex flex-col h-full">
      <ThreadPrimitive.Viewport className="flex-1 overflow-auto p-6 space-y-4">
        <ThreadPrimitive.Empty>
          <p className="text-sm text-gray-500 dark:text-gray-400">
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
          <p className="text-xs text-gray-500 dark:text-gray-400">Thinking…</p>
        </ThreadPrimitive.If>
      </ThreadPrimitive.Viewport>

      <ComposerPrimitive.Root className="flex gap-2 p-4 border-t border-gray-200 dark:border-gray-800">
        <ComposerPrimitive.Input
          autoFocus
          disabled={disabled}
          placeholder={disabled ? 'Start a session first' : 'Message the agent…'}
          className="flex-1 px-3 py-2 rounded-md border border-gray-300 dark:border-gray-700 bg-white dark:bg-gray-950 text-gray-900 dark:text-gray-100 resize-none disabled:opacity-50"
        />
        <ComposerPrimitive.Send
          disabled={disabled}
          className="flex items-center gap-2 px-4 py-2 rounded-md bg-gray-900 dark:bg-gray-100 text-white dark:text-gray-900 text-sm font-medium disabled:opacity-50"
        >
          <Send size={16} />
        </ComposerPrimitive.Send>
      </ComposerPrimitive.Root>
    </ThreadPrimitive.Root>
  )
}
