import { useEffect, useRef, useState } from 'react'
import { Download, X } from 'lucide-react'

import Markdown from 'react-markdown'
import remarkGfm from 'remark-gfm'

import { fileUrl, isMarkdown, readPreview, type Preview } from '../lib/chat'
import { ApiError } from '../lib/api'
import { iconButton } from '../lib/buttons'
import { useFocusTrap } from '../lib/useFocusTrap'

/**
 * A look at one stored file, over the conversation.
 *
 * What can be previewed is the server's decision, made from the bytes: a short
 * allowlist of text and images, and everything else refused. So this renders
 * what it is given rather than reasoning about names -- the one exception is
 * markdown, which is text either way and only differs in how it is shown.
 *
 * Anything without a preview still gets a modal. "This cannot be shown, here
 * is the download" is an answer; a row that does nothing when clicked is not.
 */
export default function FilePreview({
  sessionId,
  path,
  onClose,
}: {
  sessionId: string
  /** Scoped, as the agent would name it: `session/report.md`. */
  path: string
  onClose: () => void
}) {
  const [preview, setPreview] = useState<Preview | null>(null)
  const [error, setError] = useState<string | null>(null)
  const dialog = useRef<HTMLDivElement>(null)
  const closeButton = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    let cancelled = false
    let objectUrl: string | null = null

    void (async () => {
      try {
        const result = await readPreview(sessionId, path)
        if (cancelled) {
          // Arrived after the modal closed: the blob would leak otherwise,
          // since nothing is left to revoke it.
          if (result.kind === 'image') URL.revokeObjectURL(result.url)
          return
        }
        if (result.kind === 'image') objectUrl = result.url
        setPreview(result)
      } catch (e) {
        if (!cancelled) setError(e instanceof ApiError ? e.message : 'could not read that file')
      }
    })()

    return () => {
      cancelled = true
      if (objectUrl) URL.revokeObjectURL(objectUrl)
    }
  }, [sessionId, path])

  // Focus starts on the close button rather than the first thing in the
  // dialog, so Escape has a visible counterpart for a pointer user who tabbed
  // in.
  useFocusTrap(dialog, onClose, closeButton)

  const name = path.split('/').pop() ?? path

  return (
    // The backdrop closes on a click, which is a convenience for a pointer.
    // Keyboard users have Escape, handled by the focus trap, so this carries no key
    // handler of its own and nothing here is reachable by tab.
    <div
      className="fixed inset-0 z-40 flex items-center justify-center bg-black/50 p-4"
      onClick={(event) => {
        // Only a click on the backdrop itself. Checking the target rather
        // than stopping propagation inside means the dialog needs no click
        // handler, and nothing in it pretends to be interactive.
        if (event.target === event.currentTarget) onClose()
      }}
      role="presentation"
    >
      <div
        ref={dialog}
        role="dialog"
        aria-modal="true"
        aria-label={`Preview of ${name}`}
        className="flex flex-col w-full max-w-3xl max-h-[80vh] rounded-lg bg-white dark:bg-surface-900 border border-surface-200 dark:border-surface-700 shadow-xl"
      >
        <div className="flex items-center gap-2 px-4 py-3 border-b border-surface-200 dark:border-surface-800">
          <p
            className="flex-1 min-w-0 truncate text-sm font-medium text-surface-800 dark:text-surface-200"
            title={path}
          >
            {path}
          </p>
          <a
            href={fileUrl(sessionId, path)}
            download
            aria-label={`Download ${name}`}
            className={iconButton}
          >
            <Download size={14} aria-hidden />
          </a>
          <button
            ref={closeButton}
            type="button"
            onClick={onClose}
            aria-label="Close preview"
            className={iconButton}
          >
            <X size={16} aria-hidden />
          </button>
        </div>

        <div className="flex-1 overflow-auto p-4">
          {error && (
            <p className="text-sm text-red-600 dark:text-red-400" role="alert">
              {error}
            </p>
          )}

          {!error && preview === null && (
            <p className="text-sm text-surface-500 dark:text-surface-400">Reading…</p>
          )}

          {preview?.kind === 'none' && (
            <p className="text-sm text-surface-600 dark:text-surface-400">
              This file cannot be previewed.{' '}
              <a
                href={fileUrl(sessionId, path)}
                download
                className="text-brand-700 dark:text-brand-400 underline hover:no-underline"
              >
                Download it instead
              </a>
              .
            </p>
          )}

          {preview?.kind === 'image' && (
            <img
              src={preview.url}
              alt={`Contents of ${name}`}
              className="max-w-full mx-auto rounded"
            />
          )}

          {preview?.kind === 'text' &&
            (isMarkdown(path) ? (
              <div className="prose prose-sm dark:prose-invert max-w-none">
                {/* `react-markdown` rather than the chat renderer, which
                    reads its text from the message it is inside. Both are the
                    same library underneath. Raw HTML is not enabled, so a
                    markdown file written by an agent cannot smuggle a script
                    tag into this origin. */}
                <Markdown remarkPlugins={[remarkGfm]}>{preview.text}</Markdown>
              </div>
            ) : (
              // Monospace and wrapped: a preview of a log or a CSV is read as
              // lines, and a horizontal scrollbar makes that work.
              <pre className="text-xs font-mono whitespace-pre-wrap break-words text-surface-800 dark:text-surface-200">
                {preview.text}
              </pre>
            ))}

          {preview?.kind === 'text' && preview.truncated && (
            <p className="mt-3 text-xs text-surface-500 dark:text-surface-400">
              Showing the first 256KB.{' '}
              <a
                href={fileUrl(sessionId, path)}
                download
                className="underline hover:no-underline"
              >
                Download the whole file
              </a>
              .
            </p>
          )}
        </div>
      </div>
    </div>
  )
}
