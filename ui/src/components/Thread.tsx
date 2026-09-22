import { ComposerPrimitive, MessagePrimitive, ThreadPrimitive, useAuiState } from '@assistant-ui/react'
import { CircleSlash, Loader, Merge, RotateCw, Send, Square, X } from 'lucide-react'

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
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
 * Whether a status is one the mark can draw as movement.
 *
 * `queued`, `steering` and `waiting` are all the same news to a reader --
 * something has your message and nothing has come back -- so they are all the
 * same movement, and what separates them goes in the label. `retrying` joins
 * them because it is also a turn still in flight; it says so in words rather
 * than by moving differently, since a reader cannot be expected to tell two
 * pulse rates apart and guess which means what.
 *
 * The rest are terminal, and a mark that draws movement has nothing true to
 * say about them. They stay as badges under the prompt.
 */
function heldLabel(status: MessageStatus): string | null {
  switch (status.kind) {
    case 'queued':
      return 'Queued'
    case 'steering':
      return 'Queued: it will join the reply being written'
    case 'waiting':
      return 'Waiting for the model'
    case 'retrying':
      return 'Starting over: the runtime was lost'
    default:
      return null
  }
}

/**
 * Where a message ended up, shown under the message it is about.
 *
 * Only the states a turn can finish in. While a turn is in flight the mark on
 * the reply says so instead -- one shape for the whole life of a turn, rather
 * than a spinner here handing over to a different animation there.
 *
 * A failure gets a button, not a badge. The old red cross said what happened
 * and left the reader with nothing to do about it: the turn would not retry
 * on its own, and a refresh retried it once by accident rather than because
 * anybody asked. Choosing when to try again is the reader's to make.
 */
function StatusLine({ status, onRetry }: { status: MessageStatus; onRetry?: () => void }) {
  if (status.kind === 'failed') {
    return (
      <button
        type="button"
        onClick={onRetry}
        disabled={!onRetry}
        className="mt-1 inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-xs text-red-700 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-950/40 disabled:hover:bg-transparent disabled:cursor-default"
        title={`Failed: ${status.message}`}
        aria-label={`Failed: ${status.message}. Send it again.`}
      >
        <RotateCw size={12} aria-hidden />
        Try again
      </button>
    )
  }

  const { icon, label, tone } = describe(status)
  return (
    <span
      className={`mt-1 inline-flex items-center ${tone}`}
      title={label}
      role="status"
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
} {
  const muted = 'text-surface-500 dark:text-surface-400'
  switch (status.kind) {
    case 'absorbed':
      return {
        icon: <Merge size={14} aria-hidden />,
        label: 'Folded into the reply in progress',
        tone: muted,
      }
    case 'silent':
      // Not red: nothing failed, and dressing it as an error sends somebody
      // looking for a fault that was never recorded. What the reader needs is
      // to know the turn is over so they can say something else.
      return {
        icon: <CircleSlash size={14} aria-hidden />,
        label: 'The agent ended its turn without replying',
        tone: 'text-amber-700 dark:text-amber-400',
      }
    default:
      // Every in-flight state is drawn by the mark, not here. Unreachable
      // while `heldLabel` and this agree on which is which, and typed as
      // never so that adding a state to `MessageStatus` without deciding
      // where it belongs fails the build rather than drawing nothing.
      return { icon: null, label: '', tone: muted }
  }
}

function UserMessage({ onRetry }: { onRetry?: (text: string) => void }) {
  const status = useAuiState(
    (s) => (s.message.metadata.custom?.status as MessageStatus | null | undefined) ?? null,
  )
  const id = useAuiState((s) => s.message.id)
  // What to send again if this one failed. Read off the message rather than
  // held by the caller, because the caller does not know which message the
  // button belongs to.
  const text = useAuiState((s) =>
    s.message.content
      .filter((part): part is { type: 'text'; text: string } => part.type === 'text')
      .map((part) => part.text)
      .join(''),
  )
  const { phrase, shown, handlers } = useMessageAge(id ?? '')
  const held = status ? heldLabel(status) : null
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
      {/* In flight: the mark, breathing, which is the same mark the reply
          wears once it starts arriving. Finished: a badge, or the button to
          send it again. */}
      {held ? (
        <Working phase="held" label={held} />
      ) : (
        status && (
          <StatusLine
            status={status}
            onRetry={onRetry && text ? () => onRetry(text) : undefined}
          />
        )
      )}
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
  // The newest reply in the transcript, which is the only one that wears the
  // finished mark. Taken from the transcript rather than from what this tab
  // watched: a component only knows what it saw, so every reply it watched
  // finish would keep its line and a refresh would clear them all.
  const newest = useAuiState((s) => s.message.metadata.custom?.newest === true)

  // Kept a moment longer than it is the newest, so the line can fade rather
  // than blink off. A newer reply takes the mark the instant it exists, and
  // removing this one on the same frame draws the eye upward exactly as the
  // next reply starts arriving below it.
  const [lingering, setLingering] = useState(false)
  const wasNewest = useRef(newest)
  useEffect(() => {
    if (newest) {
      wasNewest.current = true
      setLingering(false)
      return
    }
    if (!wasNewest.current) return
    wasNewest.current = false
    setLingering(true)
    const timer = setTimeout(() => setLingering(false), 500)
    return () => clearTimeout(timer)
  }, [newest])
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
      {/* Kept mounted after the turn ends so the mark can finish: it draws its
          dots together into a line rather than vanishing, which is what
          distinguishes a reply that is done from one still being written. */}
      {(running || newest || lingering) && (
        <Working phase={running ? 'running' : 'done'} leaving={lingering} />
      )}
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

  // Put a failed message back in the box rather than sending it again behind
  // the reader's back. The turn failed for a reason they may want to act on --
  // a model that was down, a prompt that asked for too much -- and a button
  // that silently re-sent would repeat whatever caused it. This gives them the
  // words back, in the place they would have typed them, ready to edit or
  // send. The native setter, because React's own onChange does not fire for a
  // value assigned straight to the node.
  const retry = useCallback((text: string) => {
    const node = input.current
    if (!node) return
    const setter = Object.getOwnPropertyDescriptor(
      window.HTMLTextAreaElement.prototype,
      'value',
    )?.set
    setter?.call(node, text)
    node.dispatchEvent(new Event('input', { bubbles: true }))
    node.focus()
  }, [])

  // Bound once rather than inline, so every user message is not rerendered by
  // a new component identity each time this one renders.
  const UserMessageWithRetry = useMemo(
    () => function Bound() {
      return <UserMessage onRetry={retry} />
    },
    [retry],
  )

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
            UserMessage: UserMessageWithRetry,
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
