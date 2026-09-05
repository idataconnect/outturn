import { ComposerPrimitive, MessagePrimitive, ThreadPrimitive } from '@assistant-ui/react'
import { Send } from 'lucide-react'

import MarkdownText from './MarkdownText'
import ToolCall from './ToolCall'
import toolRenderers from './toolRenderers'

/**
 * Thread built from assistant-ui primitives directly, using this project's
 * Tailwind classes. The prebuilt component is distributed through shadcn's
 * generator, which would pull in path aliases, components.json and a second
 * styling convention for no benefit here.
 */

/** Shown in the reply's own bubble until its first token arrives. */
function Thinking() {
  return (
    <span className="inline-flex items-center gap-1.5 text-surface-500 dark:text-surface-400">
      <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-current" aria-hidden />
      Thinking&hellip;
    </span>
  )
}

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
        {/* Tools show the verb the model wrote. A tool that shows more has a
            renderer in `toolRenderers` written for it specifically -- the
            fallback deliberately cannot, because whatever it did show would
            arrive by default on every tool added afterwards.

            Empty fills the reply while it is still being generated. The reply
            row exists from the moment the turn starts so deltas have somewhere
            to attach, and an empty message renders as an empty bubble -- so it
            says what it is doing instead. The first delta replaces this in the
            same bubble, which is why the indicator belongs here rather than
            beside the thread: one object throughout, nothing swapped. */}
        <MessagePrimitive.Parts
          components={{
            Text: MarkdownText,
            Empty: Thinking,
            tools: { by_name: toolRenderers, Fallback: ToolCall },
          }}
        />
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
