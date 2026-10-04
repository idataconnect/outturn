import { useEffect, useState } from 'react'
import { FileCode, RefreshCw, Trash2 } from 'lucide-react'

import { ApiError } from '../lib/api'
import {
  annotate,
  getSource,
  regenerate,
  retireAnnotation,
  type Annotation,
  type Regenerated,
  type SkillSource as Source,
} from '../lib/skills'

const KIND: Record<Annotation['kind'], string> = {
  note: 'Note',
  prefer: 'Prefer instead',
  hidden: 'Hidden',
  approval: 'Needs approval',
}

const APPROVAL_TEMPLATE = 'approval:\n  requires: \n  matches: POST /\n  binds: []'

/**
 * Where a generated skill comes from, and what people added to it.
 *
 * A generated skill is not edited: its instructions and files are made from a
 * specification and the notes here, and made again when either changes, so an
 * updated specification keeps everything people added. A note goes at the
 * level where it has to be seen -- the whole skill (the instructions every
 * turn carries), a category, or one operation. Regenerating shows what would
 * change; publishing it is a version like any other.
 */
export default function SkillSource({
  skillId,
  editable,
  onDerived,
  onPublished,
}: {
  skillId: string
  /** Whether this person may change the operator's skills. */
  editable: boolean
  /** Told whether this skill is generated, so the page can stop offering to
   *  edit what would be overwritten. */
  onDerived: (derived: boolean) => void
  onPublished: () => void
}) {
  const [source, setSource] = useState<Source | null>(null)
  const [proposal, setProposal] = useState<Regenerated | null>(null)
  const [pendingSpec, setPendingSpec] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const [level, setLevel] = useState<Annotation['level']>('skill')
  const [target, setTarget] = useState('')
  const [kind, setKind] = useState<Annotation['kind']>('note')
  const [value, setValue] = useState('')

  const load = async () => {
    const s = await getSource(skillId)
    setSource(s)
    onDerived(s !== null)
  }

  useEffect(() => {
    void load().catch((e) => setError(e instanceof ApiError ? e.message : 'failed to load'))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [skillId])

  async function run(work: () => Promise<unknown>) {
    setBusy(true)
    setError(null)
    try {
      await work()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'that did not work')
    } finally {
      setBusy(false)
    }
  }

  if (!source) return null

  const outline = source.outline
  const targets =
    level === 'category'
      ? (outline?.categories ?? [])
      : level === 'operation'
        ? (outline?.operations ?? []).map((o) => o.name)
        : []
  const describe = (a: Annotation) =>
    a.level === 'skill'
      ? 'Whole skill'
      : `${a.level === 'category' ? 'Category' : 'Operation'} ${a.target}`

  return (
    <section
      className="mt-6 rounded-md border border-surface-200 dark:border-surface-800 p-4"
      aria-labelledby="skill-source"
    >
      <h2
        id="skill-source"
        className="flex items-center gap-2 text-sm font-medium text-surface-800 dark:text-surface-200"
      >
        <FileCode size={14} aria-hidden />
        Generated from a specification
      </h2>
      <p className="mt-1 text-xs text-surface-600 dark:text-surface-400">
        <span className="font-mono">{source.base_url}</span>
        {source.revision && <> · kept {new Date(source.revision.created_at).toLocaleString()}</>}.
        Its instructions and files are made from the specification and the notes below, so they are
        not edited directly: add a note here, and regenerate.
      </p>
      {error && (
        <p className="mt-2 text-xs text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      <h3 className="mt-4 text-xs font-medium text-surface-700 dark:text-surface-300">
        Notes and rules
      </h3>
      {source.annotations.length === 0 ? (
        <p className="mt-1 text-xs text-surface-500">None yet.</p>
      ) : (
        <ul className="mt-1 space-y-1">
          {source.annotations.map((a) => (
            <li key={a.id} className="flex items-start gap-2 text-xs">
              <span className="shrink-0 rounded bg-surface-100 dark:bg-surface-800 px-1.5 py-0.5">
                {KIND[a.kind]}
              </span>
              <span className="min-w-0 flex-1">
                <span className="text-surface-500">{describe(a)}: </span>
                <span className="whitespace-pre-wrap font-mono">
                  {a.kind === 'hidden' ? '' : a.value}
                </span>
              </span>
              {editable && (
                <button
                  type="button"
                  aria-label="Remove this note"
                  onClick={() =>
                    void run(async () => {
                      await retireAnnotation(skillId, a.id)
                      setProposal(null)
                      await load()
                    })
                  }
                  className="shrink-0 rounded p-0.5 text-surface-400 hover:text-red-600"
                >
                  <Trash2 size={12} aria-hidden />
                </button>
              )}
            </li>
          ))}
        </ul>
      )}

      {editable && (
        <form
          className="mt-3 space-y-2"
          onSubmit={(e) => {
            e.preventDefault()
            void run(async () => {
              await annotate(skillId, {
                level,
                target: level === 'skill' ? null : target,
                kind,
                value: kind === 'hidden' ? '' : value,
              })
              setValue('')
              setProposal(null)
              await load()
            })
          }}
        >
          <div className="flex flex-wrap gap-2 text-xs">
            <select
              aria-label="Where it applies"
              value={level}
              onChange={(e) => {
                const l = e.target.value as Annotation['level']
                setLevel(l)
                setTarget('')
                if (l !== 'operation') setKind('note')
              }}
              className="rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 px-2 py-1"
            >
              <option value="skill">Whole skill (always read)</option>
              <option value="category">A category</option>
              <option value="operation">An operation</option>
            </select>
            {level !== 'skill' && (
              <select
                aria-label="Which one"
                value={target}
                onChange={(e) => setTarget(e.target.value)}
                className="min-w-0 flex-1 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 px-2 py-1 font-mono"
              >
                <option value="">Choose…</option>
                {targets.map((t) => (
                  <option key={t} value={t}>
                    {t}
                  </option>
                ))}
              </select>
            )}
            {level === 'operation' && (
              <select
                aria-label="What it does"
                value={kind}
                onChange={(e) => {
                  const k = e.target.value as Annotation['kind']
                  setKind(k)
                  if (k === 'approval' && !value) setValue(APPROVAL_TEMPLATE)
                }}
                className="rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 px-2 py-1"
              >
                <option value="note">Note</option>
                <option value="prefer">Prefer another operation</option>
                <option value="hidden">Hide it</option>
                <option value="approval">Needs approval</option>
              </select>
            )}
          </div>
          {kind === 'prefer' ? (
            <select
              aria-label="Operation to use instead"
              value={value}
              onChange={(e) => setValue(e.target.value)}
              className="w-full rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 px-2 py-1 font-mono text-xs"
            >
              <option value="">Choose the operation to use instead…</option>
              {(outline?.operations ?? []).map((o) => (
                <option key={o.name} value={o.name}>
                  {o.name}
                </option>
              ))}
            </select>
          ) : kind !== 'hidden' ? (
            <textarea
              aria-label="Note"
              value={value}
              onChange={(e) => setValue(e.target.value)}
              rows={kind === 'approval' ? 5 : 2}
              placeholder={
                level === 'skill'
                  ? 'Said on every turn: keep it to what the agent needs before it knows where to look.'
                  : 'Read when the agent opens this.'
              }
              className="w-full rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 px-2 py-1 font-mono text-xs"
            />
          ) : null}
          <button
            type="submit"
            disabled={
              busy || (level !== 'skill' && !target) || (kind !== 'hidden' && !value.trim())
            }
            className="rounded-md border border-surface-300 dark:border-surface-700 px-3 py-1.5 text-xs disabled:opacity-50"
          >
            Add
          </button>
        </form>
      )}

      {editable && (
        <div className="mt-4 border-t border-surface-200 dark:border-surface-800 pt-3 space-y-2">
          <div className="flex flex-wrap items-center gap-2">
            <button
              type="button"
              disabled={busy}
              onClick={() =>
                void run(async () =>
                  setProposal(await regenerate(skillId, pendingSpec ? { spec: pendingSpec } : {})),
                )
              }
              className="flex items-center gap-1 rounded-md border border-surface-300 dark:border-surface-700 px-3 py-1.5 text-xs"
            >
              <RefreshCw size={12} aria-hidden />
              See what regenerating would change
            </button>
            <label className="text-xs text-surface-600 dark:text-surface-400">
              or from a newer specification:{' '}
              <input
                type="file"
                accept=".json,.yaml,.yml"
                aria-label="Newer specification"
                onChange={async (e) => {
                  const f = e.target.files?.[0]
                  setPendingSpec(f ? await f.text() : null)
                  setProposal(null)
                }}
                className="text-xs"
              />
            </label>
          </div>
          {proposal && (
            <div
              className="rounded-md bg-surface-50 dark:bg-surface-800/50 p-3 text-xs space-y-2"
              role="status"
            >
              <p>
                {proposal.body_changed
                  ? 'The instructions change. '
                  : 'The instructions are the same. '}
                {proposal.changed.length === 0
                  ? 'No files change.'
                  : `${proposal.changed.length} file${proposal.changed.length === 1 ? '' : 's'} change: ${proposal.changed
                      .slice(0, 8)
                      .map((c) => `${c.path} (${c.change})`)
                      .join(', ')}${proposal.changed.length > 8 ? ', …' : ''}.`}
              </p>
              {proposal.unmatched.length > 0 && (
                <p className="text-amber-700 dark:text-amber-500">
                  {proposal.unmatched.length} note{proposal.unmatched.length === 1 ? '' : 's'} name
                  something this specification no longer has, and will not apply:{' '}
                  {proposal.unmatched.map(describe).join(', ')}. They are kept.
                </p>
              )}
              <button
                type="button"
                disabled={busy || (!proposal.body_changed && proposal.changed.length === 0)}
                onClick={() =>
                  void run(async () => {
                    await regenerate(skillId, {
                      publish: true,
                      ...(pendingSpec ? { spec: pendingSpec } : {}),
                    })
                    setProposal(null)
                    setPendingSpec(null)
                    await load()
                    onPublished()
                  })
                }
                className="rounded-md bg-brand-700 px-3 py-1.5 font-medium text-white disabled:opacity-50"
              >
                Publish as a new version
              </button>
            </div>
          )}
        </div>
      )}
    </section>
  )
}
