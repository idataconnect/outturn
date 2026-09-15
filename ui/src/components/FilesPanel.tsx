import { useCallback, useEffect, useRef, useState } from 'react'
import { Building2, Check, ChevronDown, Clock, Download, FileText, MessagesSquare, Trash2, Upload } from 'lucide-react'

import { ApiError } from '../lib/api'
import { deleteFile, fileUrl, listFiles, uploadFile, type StoredFile } from '../lib/chat'
import FilePreview from './FilePreview'
import { useSession } from '../lib/session'
import { iconButton, iconButtonDanger } from '../lib/buttons'

type Scope = StoredFile['scope']

const SCOPES: {
  scope: Scope
  label: string
  hint: string
  icon: typeof Clock
  write: string
  read: string
}[] = [
  {
    scope: 'session',
    label: 'This conversation',
    hint: 'Short-lived files only needed for this session.',
    icon: Clock,
    write: 'sessions:create',
    read: 'sessions:read',
  },
  {
    scope: 'agent',
    label: 'This agent',
    hint: 'Files that may be used by this agent across multiple sessions',
    icon: MessagesSquare,
    write: 'storage:agent:write',
    read: 'storage:agent:read',
  },
  {
    scope: 'workspace',
    label: 'Workspace',
    hint: 'Files shared with every agent.',
    icon: Building2,
    write: 'storage:workspace:write',
    read: 'storage:workspace:read',
  },
]

function size(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

/**
 * The files a conversation can reach, and a way to add one.
 *
 * Three scopes, which are three lifetimes; the select offers only the ones
 * this person may write to, and defaults to the conversation because that is
 * where most uploads belong. A file put here is one the agent lists and reads
 * by the same name, so the panel's names are the agent's names.
 */
export default function FilesPanel({
  sessionId,
  reloadKey = 0,
}: {
  sessionId: string
  /** Changed when something outside this panel stored or removed a file, so
   *  the list shows it without anyone having to reopen the panel. A pasted
   *  image is uploaded from the composer, which knows nothing about this. */
  reloadKey?: number
}) {
  const state = useSession()
  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const writable = SCOPES.filter((s) => authorities.includes(s.write))
  const [scope, setScope] = useState<Scope>('session')
  const [files, setFiles] = useState<StoredFile[]>([])
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [menuOpen, setMenuOpen] = useState(false)
  const input = useRef<HTMLInputElement>(null)
  const menuRef = useRef<HTMLDivElement>(null)
  // Dragging over a child fires `dragleave` on the parent, so counting
  // enter against leave is what keeps the overlay from flickering as the
  // pointer crosses the file list.
  const depth = useRef(0)
  const [dropping, setDropping] = useState(false)
  /** The file being looked at, or null. */
  const [previewing, setPreviewing] = useState<string | null>(null)

  const load = useCallback(async () => {
    try {
      setFiles(await listFiles(sessionId))
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to load files')
    }
  }, [sessionId])

  useEffect(() => {
    void load()
  }, [load, reloadKey])

  // The chosen scope must be one this person may write; if their rights
  // changed under them, fall back to the first they still hold.
  useEffect(() => {
    if (writable.length > 0 && !writable.some((s) => s.scope === scope)) {
      setScope(writable[0].scope)
    }
  }, [writable, scope])

  async function onPick(list: FileList | null) {
    if (!list || list.length === 0) return
    setBusy(true)
    try {
      for (const file of Array.from(list)) {
        await uploadFile(sessionId, scope, file)
      }
      await load()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'upload failed')
    } finally {
      setBusy(false)
      if (input.current) input.current.value = ''
    }
  }

  // A drop puts files exactly where the Upload button would have: the scope
  // the select is showing. Nothing is offered to drop onto when this person
  // may write to no scope at all.
  function onDragOver(e: React.DragEvent) {
    if (writable.length === 0 || !e.dataTransfer.types.includes('Files')) return
    e.preventDefault()
    e.dataTransfer.dropEffect = 'copy'
  }

  function onDragEnter(e: React.DragEvent) {
    if (writable.length === 0 || !e.dataTransfer.types.includes('Files')) return
    depth.current += 1
    setDropping(true)
  }

  function onDragLeave() {
    depth.current = Math.max(0, depth.current - 1)
    if (depth.current === 0) setDropping(false)
  }

  function onDrop(e: React.DragEvent) {
    if (writable.length === 0) return
    e.preventDefault()
    depth.current = 0
    setDropping(false)
    void onPick(e.dataTransfer.files)
  }

  async function onDelete(file: StoredFile) {
    if (!window.confirm(`Delete ${file.path}?`)) return
    try {
      await deleteFile(sessionId, file.path)
      await load()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'delete failed')
    }
  }

  const canWrite = (s: Scope) => authorities.includes(SCOPES.find((x) => x.scope === s)!.write)

  useEffect(() => {
    if (!menuOpen) return
    function onClick(e: MouseEvent) {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setMenuOpen(false)
    }
    document.addEventListener('mousedown', onClick)
    return () => document.removeEventListener('mousedown', onClick)
  }, [menuOpen])

  const active = SCOPES.find((s) => s.scope === scope)
  const ActiveIcon = active?.icon ?? Clock

  return (
    <aside
      className="relative w-full h-full flex flex-col"
      onDragOver={onDragOver}
      onDragEnter={onDragEnter}
      onDragLeave={onDragLeave}
      onDrop={onDrop}
    >
      {dropping && (
        <div className="absolute inset-2 z-20 pointer-events-none flex flex-col items-center justify-center gap-2 rounded-lg border-2 border-dashed border-brand-500 bg-white/90 dark:bg-surface-900/90">
          <Upload size={20} className="text-brand-600 dark:text-brand-400" aria-hidden />
          <p className="px-4 text-center text-sm text-surface-700 dark:text-surface-300">
            Drop to upload to {active?.label.toLowerCase()}
          </p>
        </div>
      )}
      <div className="p-3 border-b border-surface-200 dark:border-surface-800 space-y-2">
        {writable.length > 0 && (
          <>
            <input
              ref={input}
              type="file"
              multiple
              data-testid="files-input"
              className="hidden"
              onChange={(e) => void onPick(e.target.files)}
            />
            <div className="relative flex gap-1.5" ref={menuRef}>
              <button
                type="button"
                onClick={() => setMenuOpen((v) => !v)}
                aria-haspopup="listbox"
                aria-expanded={menuOpen}
                aria-label={`Upload destination: ${active?.label}`}
                title={`${active?.label} — ${active?.hint}`}
                className="flex items-center justify-center gap-0.5 w-10 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 text-surface-600 dark:text-surface-300 hover:bg-surface-50 dark:hover:bg-surface-800/50"
              >
                <ActiveIcon size={14} aria-hidden />
                <ChevronDown size={12} aria-hidden />
              </button>
              <button
                type="button"
                disabled={busy}
                onClick={() => input.current?.click()}
                className="flex-1 flex items-center justify-center gap-2 px-3 py-1.5 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium disabled:opacity-50"
              >
                <Upload size={14} aria-hidden />
                {busy ? 'Uploading…' : 'Upload'}
              </button>

              {menuOpen && (
                <div className="absolute z-10 top-full mt-1 left-0 w-64 rounded-md border border-surface-200 dark:border-surface-700 bg-white dark:bg-surface-900 shadow-lg py-1">
                  <p className="px-3 py-1.5 text-xs font-medium text-surface-500 dark:text-surface-400 border-b border-surface-200 dark:border-surface-700">
                    Upload destination
                  </p>
                  <ul role="listbox" className="py-1">
                  {SCOPES.map((s) => {
                    const disabled = !canWrite(s.scope)
                    const selected = s.scope === scope
                    const Icon = s.icon
                    return (
                      <li key={s.scope}>
                        <button
                          type="button"
                          role="option"
                          aria-selected={selected}
                          disabled={disabled}
                          onClick={() => {
                            setScope(s.scope)
                            setMenuOpen(false)
                          }}
                          className="w-full flex items-start gap-2 px-3 py-2 text-left hover:bg-surface-50 dark:hover:bg-surface-800/50 disabled:opacity-40 disabled:hover:bg-transparent"
                        >
                          <Icon size={14} className="mt-0.5 shrink-0 text-surface-400" aria-hidden />
                          <span className="flex-1">
                            <span className="block text-sm text-surface-900 dark:text-surface-100">{s.label}</span>
                            <span className="block text-xs text-surface-500 dark:text-surface-400">{s.hint}</span>
                          </span>
                          <Check
                            size={14}
                            className={`mt-0.5 shrink-0 ${selected ? 'text-brand-600 dark:text-brand-400' : 'text-transparent'}`}
                            aria-hidden
                          />
                        </button>
                      </li>
                    )
                  })}
                  </ul>
                </div>
              )}
            </div>
          </>
        )}
      </div>

      {error && (
        <p className="px-3 py-2 text-xs text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      <div className="relative flex-1 min-h-0">
        {/* A watermark, behind the list rather than inside it: the list
            scrolls and this should not go with it. Decorative, so it is
            hidden from assistive technology and never takes a pointer. */}
        <FileText
          className="absolute inset-0 m-auto w-32 h-32 text-surface-200/60 dark:text-surface-700/40 pointer-events-none"
          strokeWidth={1}
          aria-hidden
        />
        <ul className="relative h-full overflow-auto p-2 space-y-1">
        {files.length === 0 && (
          <li className="px-2 py-1.5 text-xs text-surface-500 dark:text-surface-400">
            No files yet.
          </li>
        )}
        {files.map((f) => (
          <li
            key={f.path}
            className="group flex items-center gap-2 px-2 py-1.5 rounded-md hover:bg-surface-50 dark:hover:bg-surface-800/50"
          >
            <FileText size={14} className="shrink-0 text-surface-400" aria-hidden />
            {/* A button rather than a clickable row: this is reached by
                keyboard and announced as something that does a thing. The
                download and delete controls stay beside it, because a button
                inside a button is not a thing. */}
            <button
              type="button"
              onClick={() => setPreviewing(f.path)}
              title={f.path}
              className="flex-1 min-w-0 text-left rounded-sm focus:outline-none focus:ring-2 focus:ring-brand-500/40"
            >
              <span className="block text-sm text-surface-800 dark:text-surface-200 truncate">
                {f.path}
              </span>
              <span className="block text-xs text-surface-500 dark:text-surface-400">
                {size(f.size)}
              </span>
            </button>
            <a
              href={fileUrl(sessionId, f.path)}
              download
              aria-label={`Download ${f.path}`}
              className={iconButton}
            >
              <Download size={14} aria-hidden />
            </a>
            {canWrite(f.scope) && (
              <button
                type="button"
                onClick={() => void onDelete(f)}
                aria-label={`Delete ${f.path}`}
                className={iconButtonDanger}
              >
                <Trash2 size={14} aria-hidden />
              </button>
            )}
          </li>
        ))}
        </ul>
      </div>

      {previewing && (
        <FilePreview
          sessionId={sessionId}
          path={previewing}
          onClose={() => setPreviewing(null)}
        />
      )}
    </aside>
  )
}
