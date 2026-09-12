import { ComposerPrimitive, MessagePrimitive, ThreadPrimitive, useAuiState } from '@assistant-ui/react'
import { CircleX, Hourglass, Loader, Merge, RotateCw, Send, Square } from 'lucide-react'

import type React from 'react'

import type { MessageStatus } from '../lib/useChatRuntime'
import MarkdownText, { UserMarkdownText } from './MarkdownText'
import ToolCall from './ToolCall'
import toolRenderers from './toolRenderers'

/**
 * Thread built from assistant-ui primitives directly, using this project's
 * Tailwind classes. The prebuilt component is distributed through shadcn's
 * generator, which would pull in path aliases, components.json and a second
 * styling convention for no benefit here.
 */

/**
 * Where a message is, shown as an icon under the message it is about.
 *
 * Icon only, with the words in a tooltip and read out to a screen reader.
 * Until the first token there is nothing of the reply to show, and a message
 * taken mid-turn never gets a reply of its own, so the state sits on the
 * user's message. Colour is a secondary cue; the icon carries the meaning.
 */
function StatusLine({ status }: { status: MessageStatus }) {
  const { icon, label, tone, live } = describe(status)
  return (
    <span
      className={`mt-1 inline-flex items-center ${tone}`}
      title={label}
      role={live}
      aria-label={label}
    >
      {icon}
    </span>
  )
}

function describe(status: MessageStatus): {
  icon: React.ReactNode
  label: string
  tone: string
  live: 'status' | 'alert'
} {
  const muted = 'text-surface-500 dark:text-surface-400'
  switch (status.kind) {
    case 'queued':
    case 'steering':
      return {
        icon: <Hourglass size={14} aria-hidden />,
        label: 'Queued',
        tone: muted,
        live: 'status',
      }
    case 'waiting':
      return {
        icon: <Loader size={14} className="animate-spin" aria-hidden />,
        label: 'Waiting for the model',
        tone: muted,
        live: 'status',
      }
    case 'retrying':
      return {
        icon: <RotateCw size={14} className="animate-spin" aria-hidden />,
        label: 'Starting over: the runtime was lost',
        tone: 'text-amber-700 dark:text-amber-400',
        live: 'status',
      }
    case 'absorbed':
      return {
        icon: <Merge size={14} aria-hidden />,
        label: 'Folded into the reply in progress',
        tone: muted,
        live: 'status',
      }
    case 'failed':
      return {
        icon: <CircleX size={14} aria-hidden />,
        label: `Failed: ${status.message}`,
        tone: 'text-red-700 dark:text-red-400',
        live: 'alert',
      }
  }
}

function UserMessage() {
  const status = useAuiState(
    (s) => (s.message.metadata.custom?.status as MessageStatus | null | undefined) ?? null,
  )
  return (
    <MessagePrimitive.Root className="flex flex-col items-end">
      <div className="group user max-w-[75%] px-4 py-2 rounded-lg text-sm bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white">
        <MessagePrimitive.Parts components={{ Text: UserMarkdownText }} />
      </div>
      {status && <StatusLine status={status} />}
    </MessagePrimitive.Root>
  )
}

/** Nothing: an empty reply says its state under the prompt instead. */
function Nothing() {
  return null
}

function AssistantMessage() {
  // A reply with nothing in it yet is not drawn. The row exists from the
  // moment the turn starts so deltas have somewhere to attach, but an empty
  // bubble tells the reader nothing true -- the status under their own
  // message does. The first delta or tool call makes it appear.
  const empty = useAuiState((s) =>
    s.message.content.every((part) => part.type === 'text' && part.text === ''),
  )
  if (empty) return null

  return (
    <MessagePrimitive.Root className="flex justify-start">
      <div className="max-w-[75%] px-4 py-2 rounded-lg text-sm bg-white dark:bg-surface-900 border border-surface-200 dark:border-surface-800 text-surface-900 dark:text-surface-100">
        {/* Tools show the verb the model wrote. A tool that shows more has a
            renderer in `toolRenderers` written for it specifically -- the
            fallback deliberately cannot, because whatever it did show would
            arrive by default on every tool added afterwards. */}
        <MessagePrimitive.Parts
          components={{
            Text: MarkdownText,
            Empty: Nothing,
            tools: { by_name: toolRenderers, Fallback: ToolCall },
          }}
        />
      </div>
    </MessagePrimitive.Root>
  )
}

export default function Thread({
  disabled,
  stopping,
}: {
  disabled?: boolean
  /** A stop has been asked for and the turn has not ended yet. */
  stopping?: boolean
}) {
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
          className="flex-1 px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100 resize-none disabled:opacity-50"
        />
        {/* One button, two jobs. While a turn is running the send button is
            not merely unavailable -- there is something better to do with that
            square of screen, and a person reaching for it mid-reply is usually
            reaching to stop it. Both are rendered and assistant-ui shows
            whichever the run state calls for. */}
        <ComposerPrimitive.Send
          disabled={disabled}
          className="flex items-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium disabled:opacity-50"
        >
          <Send size={16} />
        </ComposerPrimitive.Send>
        <ComposerPrimitive.Cancel
          aria-label={stopping ? 'Stopping' : 'Stop'}
          disabled={stopping}
          title={
            stopping
              ? 'Stopping at the end of the current step'
              : 'Stop this reply'
          }
          className="flex items-center gap-2 px-4 py-2 rounded-md border border-surface-300 dark:border-surface-600 bg-white dark:bg-surface-700 text-surface-700 dark:text-surface-100 text-sm font-medium hover:bg-surface-50 dark:hover:bg-surface-600 disabled:opacity-60"
        >
          {/* The square is the universal "stop", and it keeps spinning while
              the request is in flight: the turn ends at its next step, not the
              instant the button is pressed, and a control that went still
              immediately would promise something the system cannot do. */}
          {stopping ? <Loader size={16} className="animate-spin" /> : <Square size={16} />}
        </ComposerPrimitive.Cancel>
      </ComposerPrimitive.Root>
    </ThreadPrimitive.Root>
  )
}
