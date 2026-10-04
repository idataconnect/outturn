import { memo, useCallback, useMemo, useRef, useState } from 'react'
import { AlertTriangle, FilePlus, FileText, ShieldCheck, Trash2, Upload } from 'lucide-react'

import {
  MAX_FILE_BYTES,
  declaresApproval,
  draftFile,
  reachable,
  pathProblem,
  type DraftFile,
} from '../lib/skills'

/**
 * The files a skill carries beside its main instructions.
 *
 * The split is the whole point of a package. The main instructions go to the
 * agent on every turn; these never do. The agent reads one only when the main
 * instructions tell it to, so they are the table of contents and a file they
 * never mention is a file the agent will never know is there. That is the
 * warning this list is built around.
 *
 * Edited as a set and published with the version, like the instructions: a
 * change here is a new version, and an old version keeps the files it had.
 */
export default function SkillFiles({
  files,
  onChange,
  body,
  slug,
  editable,
  onOpen,
}: {
  files: DraftFile[]
  onChange: (files: DraftFile[]) => void
  /** The main instructions, to say which files they never mention. */
  body: string
  slug: string
  editable: boolean
  /** A file was opened, so its text is wanted before the rest. Keep it
   *  stable: a new one each render re-renders every row. */
  onOpen?: (path: string) => void
}) {
  const [selected, setSelected] = useState<string | null>(files[0]?.path ?? null)
  const [pathDraft, setPathDraft] = useState<string | null>(null)
  const upload = useRef<HTMLInputElement>(null)

  // Falls back to the first file when the selected one is gone -- removed, or
  // not in the version just restored -- rather than showing an empty editor.
  const current = files.find((f) => f.path === selected) ?? files[0] ?? null
  const { reached, complete } = useMemo(() => reachable(body, files), [body, files])
  const unreached = (path: string) => complete && !reached.has(path)

  // Stable, so a row's props change only when that row does: a click then
  // re-renders the two rows whose selection moved rather than every one of
  // hundreds.
  const pick = useCallback(
    (path: string) => {
      setSelected(path)
      setPathDraft(null)
      onOpen?.(path)
    },
    [onOpen],
  )

  function edit(path: string, content: string) {
    onChange(files.map((f) => (f.path === path ? draftFile(path, content) : f)))
  }

  /** A new name is one any other file might mention, which the links the API
   *  recorded cannot know about, so every file's are worked out here again. */
  function withNewPaths(next: DraftFile[]): DraftFile[] {
    return next.map((f) => (f.links === null ? f : { ...f, links: null }))
  }

  function add() {
    let n = files.length + 1
    let path = `operation-${n}.md`
    while (files.some((f) => f.path === path)) path = `operation-${++n}.md`
    onChange(withNewPaths([...files, draftFile(path, '')]))
    setSelected(path)
  }

  function remove(path: string) {
    const rest = files.filter((f) => f.path !== path)
    onChange(rest)
    setSelected(rest[0]?.path ?? null)
  }

  /** Renamed only on leaving the field, and only to a path the API would
   *  accept: renaming on every keystroke would move the selection out from
   *  under the reader the moment a path briefly clashed with another. */
  function rename(from: string, to: string) {
    setPathDraft(null)
    if (to === from || pathProblem(to, files.map((f) => f.path).filter((p) => p !== from))) {
      return
    }
    onChange(withNewPaths(files.map((f) => (f.path === from ? { ...f, path: to } : f))))
    setSelected(to)
  }

  async function fromDisk(list: FileList | null) {
    if (!list) return
    const read = await Promise.all(
      Array.from(list).map(async (file) => draftFile(file.name, await file.text())),
    )
    // A file of the same name replaces the one here, which is what dropping in
    // a newer copy of an operation's file means.
    const incoming = new Map(read.map((f) => [f.path, f]))
    onChange(withNewPaths([...files.filter((f) => !incoming.has(f.path)), ...read]))
    setSelected(read[0]?.path ?? selected)
  }

  const draftProblem =
    current && pathDraft !== null
      ? pathProblem(
          pathDraft,
          files.map((f) => f.path).filter((p) => p !== current.path),
        )
      : null

  return (
    <section aria-labelledby="skill-files">
      <div className="flex items-baseline justify-between gap-3">
        <h2
          id="skill-files"
          className="text-sm font-medium text-surface-800 dark:text-surface-200"
        >
          Files
        </h2>
        {editable && (
          <div className="flex items-center gap-1">
            <button
              type="button"
              onClick={add}
              className="flex items-center gap-1 px-2 py-1 rounded text-xs text-surface-700 dark:text-surface-300 hover:bg-surface-100 dark:hover:bg-surface-800"
            >
              <FilePlus size={14} aria-hidden />
              New file
            </button>
            <button
              type="button"
              onClick={() => upload.current?.click()}
              className="flex items-center gap-1 px-2 py-1 rounded text-xs text-surface-700 dark:text-surface-300 hover:bg-surface-100 dark:hover:bg-surface-800"
            >
              <Upload size={14} aria-hidden />
              Upload
            </button>
            <input
              ref={upload}
              type="file"
              multiple
              accept=".md,.txt,.markdown,text/*"
              className="hidden"
              aria-label="Upload files"
              onChange={(e) => {
                void fromDisk(e.target.files)
                e.target.value = ''
              }}
            />
          </div>
        )}
      </div>
      <p className="mt-1 text-xs text-surface-600 dark:text-surface-400">
        Detail the agent reads only when it needs it, such as how to make one call. Nothing
        here is sent unless your instructions above point to it, so name each file there
        and say when to read it.
      </p>

      {files.length === 0 ? (
        <p className="mt-3 rounded-md border border-dashed border-surface-300 dark:border-surface-700 px-3 py-4 text-center text-xs text-surface-500 dark:text-surface-400">
          No files. Everything this skill says is in the instructions above.
        </p>
      ) : (
        <div className="mt-3 grid grid-cols-[minmax(0,14rem)_minmax(0,1fr)] items-start gap-3">
          {/* Scrolls on its own: a generated skill carries hundreds of files,
              and a list that grew the page left the editor beside its top. */}
          <ul className="max-h-[28rem] overflow-y-auto space-y-0.5" aria-label="Files">
            {files.map((f) => (
              <FileRow
                key={f.path}
                file={f}
                selected={f.path === current?.path}
                hidden={unreached(f.path)}
                onPick={pick}
              />
            ))}
          </ul>

          {current && (
            <div className="min-w-0 space-y-2">
              <input
                value={pathDraft ?? current.path}
                disabled={!editable || current.content === null}
                aria-label="File name"
                onChange={(e) => setPathDraft(e.target.value)}
                onBlur={() => pathDraft !== null && rename(current.path, pathDraft)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    e.preventDefault()
                    if (pathDraft !== null) rename(current.path, pathDraft)
                  }
                }}
                className="w-full px-2 py-1 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 text-xs font-mono text-surface-900 dark:text-surface-100 disabled:opacity-60"
              />
              {draftProblem && (
                <p className="text-xs text-red-600 dark:text-red-400" role="alert">
                  That name {draftProblem}.
                </p>
              )}

              {unreached(current.path) && (
                <p
                  className="flex items-start gap-1.5 text-xs text-amber-800 dark:text-amber-400"
                  role="status"
                >
                  <AlertTriangle size={12} className="mt-0.5 shrink-0" aria-hidden />
                  <span>
                    Your instructions never mention this file, nor does any file they lead to,
                    so the agent will not know to read it. Name it there as{' '}
                    <code className="font-mono">
                      skill/{slug || '…'}/{current.path}
                    </code>
                    .
                  </span>
                </p>
              )}
              {current.bytes > MAX_FILE_BYTES && (
                <p className="text-xs text-red-600 dark:text-red-400" role="alert">
                  Larger than {MAX_FILE_BYTES / 1024} KB, which is the most an agent reads in
                  one go. Split it into files for separate operations.
                </p>
              )}
              {current.content !== null && declaresApproval(current.content) && (
                <p className="flex items-start gap-1.5 text-xs text-surface-600 dark:text-surface-400">
                  <ShieldCheck size={12} className="mt-0.5 shrink-0" aria-hidden />
                  <span>
                    Declares an approval: the call it documents waits for somebody to say yes.
                  </span>
                </p>
              )}

              <textarea
                value={current.content ?? ''}
                placeholder={current.content === null ? 'Loading…' : undefined}
                disabled={!editable || current.content === null}
                aria-label={`Contents of ${current.path}`}
                onChange={(e) => edit(current.path, e.target.value)}
                rows={14}
                className="w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 text-xs font-mono text-surface-900 dark:text-surface-100 disabled:opacity-60"
              />
              {editable && (
                <button
                  type="button"
                  onClick={() => remove(current.path)}
                  className="flex items-center gap-1 px-2 py-1 rounded text-xs text-surface-600 dark:text-surface-400 hover:bg-surface-100 dark:hover:bg-surface-800"
                >
                  <Trash2 size={12} aria-hidden />
                  Remove this file
                </button>
              )}
            </div>
          )}
        </div>
      )}
    </section>
  )
}

/** One file in the list. */
const FileRow = memo(function FileRow({
  file,
  selected,
  hidden,
  onPick,
}: {
  file: DraftFile
  selected: boolean
  /** Nothing leads an agent to it. */
  hidden: boolean
  onPick: (path: string) => void
}) {
  const tooBig = file.bytes > MAX_FILE_BYTES
  const gated = file.content !== null && declaresApproval(file.content)
  return (
    <li>
      <button
        type="button"
        onClick={() => onPick(file.path)}
        aria-current={selected ? 'true' : undefined}
        className={`w-full flex items-center gap-1.5 rounded px-2 py-1 text-left text-xs font-mono ${
          selected
            ? 'bg-surface-100 dark:bg-surface-800 text-surface-900 dark:text-surface-100'
            : 'text-surface-700 dark:text-surface-300 hover:bg-surface-50 dark:hover:bg-surface-800/60'
        }`}
      >
        <FileText size={12} className="shrink-0" aria-hidden />
        <span className="truncate" title={file.path}>
          {file.path}
        </span>
        {gated && (
          <ShieldCheck
            size={12}
            className="ml-auto shrink-0 text-brand-600 dark:text-brand-400"
            aria-label="needs approval"
            role="img"
          />
        )}
        {(hidden || tooBig) && (
          <AlertTriangle
            size={12}
            className={`${gated ? '' : 'ml-auto '}shrink-0 text-amber-600 dark:text-amber-400`}
            aria-label={tooBig ? 'too large' : 'not mentioned in the instructions'}
            role="img"
          />
        )}
      </button>
    </li>
  )
})
