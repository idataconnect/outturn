import { ComposerPrimitive, MessagePrimitive, ThreadPrimitive, useAuiState } from '@assistant-ui/react'
import { CircleSlash, CircleX, Hourglass, Loader, Merge, RotateCw, Send, Square, X } from 'lucide-react'

import { useEffect, useRef, useState } from 'react'
import type React from 'react'

import { useMessageAge } from '../lib/useMessageAge'
import type { MessageStatus } from '../lib/useChatRuntime'
import MarkdownText, { UserMarkdownText } from './MarkdownText'
import MessageAge from './MessageAge'
import ToolCall from './ToolCall'
import toolRenderers from './toolRenderers'
import Working from './Working'
import { deleteFile, uploadPastedImage } from '../lib/chat'
import { ApiError } from '../lib/api'

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
    case 'silent':
      // Not red: nothing failed, and dressing it as an error sends somebody
      // looking for a fault that was never recorded. What the reader needs is
      // to know the turn is over so they can say something else.
      return {
        icon: <CircleSlash size={14} aria-hidden />,
        label: 'The agent ended its turn without replying',
        tone: 'text-amber-700 dark:text-amber-400',
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
  const id = useAuiState((s) => s.message.id)
  const { phrase, shown, handlers } = useMessageAge(id ?? '')
  return (
    <MessagePrimitive.Root className="flex flex-col items-end">
      <div
        {...handlers}
        tabIndex={-1}
        className="group user max-w-[75%] px-4 py-2 rounded-lg text-sm bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white"
      >
        <MessagePrimitive.Parts components={{ Text: UserMarkdownText }} />
      </div>
      <MessageAge phrase={phrase} shown={shown} align="right" />
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
  // A compaction summary, which the agent wrote about the conversation rather
  // than said in it. Drawn across the width as a marked boundary instead of as
  // a reply: everything above it is what the agent still remembers of what
  // came before, and a reader who cannot see that their conversation was
  // compacted cannot tell why it stopped referring to something.
  const summary = useAuiState((s) => s.message.metadata.custom?.summary === true)
  // Whether this reply's turn is still going. Said under the reply rather than
  // under the prompt, because by now the reply exists: the gap this covers is
  // the one after some text has arrived, where a tool call is being set up and
  // nothing streams. `StatusLine`'s `waiting` has ended by then, and without
  // this a half-finished reply is indistinguishable from a finished one.
  const running = useAuiState((s) => s.message.status?.type === 'running')
  const id = useAuiState((s) => s.message.id)
  // Hooks run before the early returns below, so the age is wired up whether or
  // not this particular message ends up drawn.
  const { phrase, shown, handlers } = useMessageAge(id ?? '')
  if (empty) return null

  if (summary) {
    return (
      <MessagePrimitive.Root className="flex justify-center">
        <div className="w-full my-2 px-4 py-2 rounded-lg text-xs bg-surface-50 dark:bg-surface-900/60 border border-dashed border-surface-300 dark:border-surface-700 text-surface-600 dark:text-surface-400">
          <p className="font-medium mb-1 uppercase tracking-wide text-[10px]">
            Earlier messages, summarised
          </p>
          <MessagePrimitive.Parts components={{ Text: MarkdownText, Empty: Nothing }} />
        </div>
      </MessagePrimitive.Root>
    )
  }

  return (
    <MessagePrimitive.Root className="flex flex-col items-start">
      <div
        {...handlers}
        tabIndex={-1}
        className="max-w-[75%] px-4 py-2 rounded-lg text-sm bg-white dark:bg-surface-900 border border-surface-200 dark:border-surface-800 text-surface-900 dark:text-surface-100"
      >
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
      <MessageAge phrase={phrase} shown={shown} />
      {running && <Working />}
    </MessagePrimitive.Root>
  )
}

/** An image pasted into this message, stored but not yet sent. */
type Attachment = {
  /** Where it was stored, which is what the model is told to look at. */
  path: string
  /** A local object URL, so the preview costs no round trip. */
  preview: string
}

export default function Thread({
  disabled,
  stopping,
  focusRequest = 0,
  sessionId,
  onStoredChange,
  takeAttachments,
}: {
  disabled?: boolean
  /** A stop has been asked for and the turn has not ended yet. */
  stopping?: boolean
  /** Changed to put the cursor in the composer, e.g. for a session just chosen. */
  focusRequest?: number
  /** Where a pasted image is stored. Absent before a session exists, which is
   *  also when there is nowhere to put one. */
  sessionId?: string | null
  /** Told when a pasted image is stored or removed, so the files panel can
   *  show what the composer just did to it. */
  onStoredChange?: (error?: string) => void
  /** Filled with a function the runtime calls at send, to collect what this
   *  composer has attached. Held by the page because the runtime is created
   *  there, while the attachments live here with the composer that made them. */
  takeAttachments?: React.MutableRefObject<(() => string) | null>
}) {
  // `autoFocus` only speaks for the first mount, and the thread outlives
  // every change of session -- so a session chosen from the sidebar left
  // the cursor on the link that chose it, and the first thing anyone did
  // was click into the box. Held until the composer is enabled, because the
  // first session of all enables it a render after the request is made.
  const input = useRef<HTMLTextAreaElement>(null)
  const focused = useRef(0)
  const [pasting, setPasting] = useState(false)
  /** Images pasted into this message and not yet sent, with a local preview. */
  const [attached, setAttached] = useState<Attachment[]>([])

  // Which of the two buttons the composer offers. assistant-ui's own
  // `isEmpty` counts its attachments, which are always none here: a pasted
  // image is stored as a session file and referenced by path, so the only
  // record that one is waiting is `attached`. Counting it is what keeps the
  // send button offered to somebody who pasted a screenshot and typed
  // nothing.
  const composerEmpty = useAuiState((s) => s.composer.isEmpty) && attached.length === 0
  // Stop is what an empty composer offers mid-run, and the only state in
  // which the send button is not the more useful of the two. `canCancel`
  // rather than the thread's `isRunning`, so the stop is never shown by a
  // runtime that could not honour it.
  const canCancel = useAuiState((s) => s.composer.canCancel)
  const showSend = !canCancel || !composerEmpty

  // The previews are object URLs, which the browser holds until they are
  // revoked. Left alone they accumulate for as long as the tab is open.
  useEffect(() => {
    return () => {
      for (const image of attached) URL.revokeObjectURL(image.preview)
    }
    // Only on unmount: revoking on every change would kill previews still
    // being shown, and each chip revokes its own when it is removed.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  // Sending is what turns an attachment into part of the message: the model
  // is told the path, which is what `describe_image` takes, so it can look
  // without anyone having to type the name out. Handed to the runtime rather
  // than called here, because the composer's submit goes through it.
  useEffect(() => {
    if (!takeAttachments) return
    takeAttachments.current = () => {
      const references = attached.map((image) => `[image: ${image.path}]`).join('\n')
      // Forgotten as they are taken: they belong to the message just sent,
      // and the previews are no longer anybody's to show.
      for (const image of attached) URL.revokeObjectURL(image.preview)
      setAttached([])
      return references
    }
    return () => {
      if (takeAttachments) takeAttachments.current = null
    }
  }, [attached, takeAttachments])

  async function removeAttachment(image: Attachment) {
    setAttached((current) => current.filter((a) => a.path !== image.path))
    URL.revokeObjectURL(image.preview)
    // Removed means removed. The file is already stored, and leaving it would
    // mean a screenshot somebody pasted by mistake is still there for the
    // agent to read -- the surprise in the direction that matters.
    if (sessionId) {
      try {
        await deleteFile(sessionId, image.path)
        onStoredChange?.()
      } catch (e) {
        onStoredChange?.(e instanceof ApiError ? e.message : 'could not remove that image')
      }
    }
  }

  // An image on the clipboard is a Blob with no name, so it cannot go through
  // the file path that drag-and-drop uses. It is stored the moment it is
  // pasted rather than held in the composer: the agent reads it by path, so
  // something that exists is something it can be asked about, and a paste
  // that vanished when the tab closed would be worse than one that landed
  // somewhere visible in the files panel.
  async function onPaste(event: React.ClipboardEvent<HTMLTextAreaElement>) {
    const images = Array.from(event.clipboardData?.items ?? []).filter(
      (item) => item.kind === 'file' && item.type.startsWith('image/'),
    )
    if (images.length === 0) return
    // Only once there is an image: a paste of ordinary text must land in the
    // box as text, and calling preventDefault on everything would eat it.
    event.preventDefault()
    if (!sessionId) return

    setPasting(true)
    try {
      for (const item of images) {
        const blob = item.getAsFile()
        if (!blob) continue
        try {
          const stored = await uploadPastedImage(sessionId, blob)
          setAttached((current) => [
            ...current,
            { path: stored.path, preview: URL.createObjectURL(blob) },
          ])
          onStoredChange?.()
        } catch (e) {
          onStoredChange?.(e instanceof ApiError ? e.message : 'could not store that image')
        }
      }
    } finally {
      setPasting(false)
    }
  }
  useEffect(() => {
    if (disabled || focusRequest === focused.current) return
    focused.current = focusRequest
    input.current?.focus()
  }, [focusRequest, disabled])

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

      {attached.length > 0 && (
        <div className="flex gap-2 flex-wrap px-4 pt-3 border-t border-surface-200 dark:border-surface-800">
          {attached.map((image) => (
            <div key={image.path} className="relative group">
              <img
                src={image.preview}
                alt={`Pasted, stored as ${image.path}`}
                className="h-16 w-16 object-cover rounded-md border border-surface-300 dark:border-surface-700"
              />
              <button
                type="button"
                onClick={() => void removeAttachment(image)}
                aria-label={`Remove ${image.path}`}
                title="Remove, and delete the stored file"
                className="absolute -top-1.5 -right-1.5 rounded-full bg-surface-800 dark:bg-surface-200 text-white dark:text-surface-900 p-0.5 opacity-90 hover:opacity-100"
              >
                <X size={12} aria-hidden />
              </button>
            </div>
          ))}
        </div>
      )}

      <ComposerPrimitive.Root className={`flex gap-2 p-4 ${attached.length > 0 ? '' : 'border-t border-surface-200 dark:border-surface-800'}`}>
        <ComposerPrimitive.Input
          ref={input}
          autoFocus
          disabled={disabled}
          // Handled here rather than by the composer's own attachment path,
          // which this project does not use: an image is stored as a session
          // file and named by path, not carried along with the message.
          addAttachmentOnPaste={false}
          onPaste={onPaste}
          placeholder={
            pasting
              ? 'Storing the image…'
              : disabled
                ? 'Start a session first'
                : 'Message the agent…'
          }
          className="flex-1 px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100 resize-none disabled:opacity-50"
        />
        {/* One square of screen, two jobs, chosen by what is in the box
            rather than by the run state alone. An empty composer mid-reply
            has nothing to send, so it offers the stop; anything typed or
            pasted is worth sending even mid-reply, because a message sent
            then steers the turn at its next round boundary rather than
            waiting for a runtime. Send is what that person is reaching for,
            and clearing the box brings the stop back. */}
        {showSend ? (
          <ComposerPrimitive.Send
            // Named, because the icon is the only thing in it and this square
            // changes which button it is: somebody listening rather than
            // looking is told what it became.
            aria-label="Send"
            disabled={disabled || composerEmpty}
            className="flex items-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium disabled:opacity-50"
          >
            <Send size={16} />
          </ComposerPrimitive.Send>
        ) : (
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
        )}
      </ComposerPrimitive.Root>
    </ThreadPrimitive.Root>
  )
}
