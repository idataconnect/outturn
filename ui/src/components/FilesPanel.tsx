import { useCallback, useEffect, useRef, useState } from 'react'
import { Download, FileText, Trash2, Upload } from 'lucide-react'

import { ApiError } from '../lib/api'
import { deleteFile, fileUrl, listFiles, uploadFile, type StoredFile } from '../lib/chat'
import { useSession } from '../lib/session'

type Scope = StoredFile['scope']

const SCOPES: { scope: Scope; label: string; hint: string; write: string; read: string }[] = [
  {
    scope: 'session',
    label: 'This conversation',
    hint: 'Swept after a while. Where files for right now go.',
    write: 'sessions:create',
    read: 'sessions:read',
  },
  {
    scope: 'agent',
    label: 'This agent',
    hint: 'Kept while the agent exists, across conversations.',
    write: 'storage:agent:write',
    read: 'storage:agent:read',
  },
  {
    scope: 'tenant',
    label: 'Workspace',
    hint: 'Shared by every agent here. Kept until deleted.',
    write: 'storage:tenant:write',
    read: 'storage:tenant:read',
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
export default function FilesPanel({ sessionId }: { sessionId: string }) {
  const state = useSession()
  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const writable = SCOPES.filter((s) => authorities.includes(s.write))
  const [scope, setScope] = useState<Scope>('session')
  const [files, setFiles] = useState<StoredFile[]>([])
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const input = useRef<HTMLInputElement>(null)

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
  }, [load])

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

  return (
    <aside className="w-72 border-l border-surface-200 dark:border-surface-800 flex flex-col">
      <div className="p-3 border-b border-surface-200 dark:border-surface-800 space-y-2">
        <p className="text-xs font-medium text-surface-600 dark:text-surface-400">Files</p>
        {writable.length > 0 && (
          <>
            <label className="block">
              <span className="sr-only">Where to put uploads</span>
              <select
                value={scope}
                onChange={(e) => setScope(e.target.value as Scope)}
                title={SCOPES.find((s) => s.scope === scope)?.hint}
                className="w-full px-2 py-1.5 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 text-sm text-surface-900 dark:text-surface-100"
              >
                {SCOPES.map((s) => (
                  <option key={s.scope} value={s.scope} disabled={!canWrite(s.scope)}>
                    {s.label}
                  </option>
                ))}
              </select>
            </label>
            <p className="text-xs text-surface-500 dark:text-surface-400">
              {SCOPES.find((s) => s.scope === scope)?.hint}
            </p>
            <input
              ref={input}
              type="file"
              multiple
              className="hidden"
              onChange={(e) => void onPick(e.target.files)}
            />
            <button
              type="button"
              disabled={busy}
              onClick={() => input.current?.click()}
              className="w-full flex items-center justify-center gap-2 px-3 py-1.5 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium disabled:opacity-50"
            >
              <Upload size={14} aria-hidden />
              {busy ? 'Uploading…' : 'Upload'}
            </button>
          </>
        )}
      </div>

      {error && (
        <p className="px-3 py-2 text-xs text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      <ul className="flex-1 overflow-auto p-2 space-y-1">
        {files.length === 0 && (
          <li className="px-2 py-1.5 text-xs text-surface-500 dark:text-surface-400">
            No files yet. The agent will see anything uploaded here under the same name.
          </li>
        )}
        {files.map((f) => (
          <li
            key={f.path}
            className="group flex items-center gap-2 px-2 py-1.5 rounded-md hover:bg-surface-50 dark:hover:bg-surface-800/50"
          >
            <FileText size={14} className="shrink-0 text-surface-400" aria-hidden />
            <div className="flex-1 min-w-0">
              <p className="text-sm text-surface-800 dark:text-surface-200 truncate" title={f.path}>
                {f.path}
              </p>
              <p className="text-xs text-surface-500 dark:text-surface-400">{size(f.size)}</p>
            </div>
            <a
              href={fileUrl(sessionId, f.path)}
              download
              aria-label={`Download ${f.path}`}
              className="p-1 rounded text-surface-400 hover:text-surface-900 dark:hover:text-surface-100"
            >
              <Download size={14} aria-hidden />
            </a>
            {canWrite(f.scope) && (
              <button
                type="button"
                onClick={() => void onDelete(f)}
                aria-label={`Delete ${f.path}`}
                className="p-1 rounded text-surface-400 hover:text-red-600 dark:hover:text-red-400"
              >
                <Trash2 size={14} aria-hidden />
              </button>
            )}
          </li>
        ))}
      </ul>
    </aside>
  )
}
