import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router'
import { AlertTriangle, ArrowLeft, GitFork, History, Layers } from 'lucide-react'

import { ApiError } from '../lib/api'
import {
  createSkill,
  forkSkill,
  getSkill,
  listSkills,
  listVersions,
  publishVersion,
  retireSkill,
  slugify,
  updateSkill,
  type Skill,
  type SkillVersion,
} from '../lib/skills'
import { useSession } from '../lib/session'

/**
 * One page for writing a skill and for changing one.
 *
 * The operator's skills open here read-only, with the two ways of varying one
 * offered instead of a save button: an override, which stays tied to what the
 * operator ships, or a fork, which does not. Which of those a reader wants is
 * the only real decision on this page, so it is the one the page is arranged
 * around.
 */
export default function SkillEditor() {
  const state = useSession()
  const navigate = useNavigate()
  const { id } = useParams<{ id?: string }>()
  const [params] = useSearchParams()
  const creating = id === undefined
  // `?override=` arrives from the button on somebody else's skill.
  const overriding = params.get('override')

  const workspaceId = state.status === 'authenticated' ? state.session.workspace_id : null
  const isOperator =
    state.status === 'authenticated' && state.session.roles.includes('system_admin')
  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canWrite = authorities.includes('skills:write')

  const [skill, setSkill] = useState<Skill | null>(null)
  const [base, setBase] = useState<Skill | null>(null)
  const [versions, setVersions] = useState<SkillVersion[]>([])
  const [form, setForm] = useState({ name: '', slug: '', description: '', body: '', note: '' })
  const [slugEdited, setSlugEdited] = useState(false)
  // Only an operator sees this, and only when writing something new.
  const [forEveryone, setForEveryone] = useState(false)
  const [loading, setLoading] = useState(!creating)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  // The operator's skills are read here and written through their own routes,
  // so the same page serves both and the difference is who is looking.
  const operators = skill !== null && skill.workspace_id !== workspaceId
  const editable = canWrite && (creating || !operators || isOperator)

  useEffect(() => {
    if (creating) {
      if (!overriding) return
      void (async () => {
        try {
          const b = await getSkill(overriding)
          setBase(b)
          setForm((f) => ({
            ...f,
            name: `${b.name} (ours)`,
            slug: slugify(`${b.slug}-ours`),
          }))
        } catch {
          setError('that skill could not be read')
        }
      })()
      return
    }
    void (async () => {
      try {
        const [s, v, all] = await Promise.all([getSkill(id), listVersions(id), listSkills()])
        setSkill(s)
        setVersions(v)
        setBase(all.find((x) => x.id === s.base_skill_id) ?? null)
        setForm({
          name: s.name,
          slug: s.slug,
          description: s.description,
          body: v[0]?.body ?? '',
          note: '',
        })
        setError(null)
      } catch (e) {
        setError(e instanceof ApiError ? e.message : 'failed to load skill')
      } finally {
        setLoading(false)
      }
    })()
  }, [id, creating, overriding])

  async function onCreate(event: React.FormEvent) {
    event.preventDefault()
    setSaving(true)
    try {
      const made = await createSkill(
        {
          slug: form.slug,
          name: form.name,
          description: form.description,
          body: form.body,
          ...(overriding ? { base_skill_id: overriding } : {}),
        },
        // An override always belongs to the workspace that wrote it, whoever
        // is signed in: it is that workspace's variation, not the operator's.
        forEveryone && !overriding,
      )
      setSaving(false)
      void navigate(`/skills/${made.id}`)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to create skill')
      setSaving(false)
    }
  }

  /** Details change in place; prose only ever moves forward. */
  async function onPublish(event: React.FormEvent) {
    event.preventDefault()
    if (!skill) return
    setSaving(true)
    try {
      if (form.name !== skill.name || form.description !== skill.description) {
        await updateSkill(
          skill.id,
          { name: form.name, description: form.description },
          operators,
        )
      }
      const live = versions[0]
      if (!live || live.body !== form.body) {
        await publishVersion(skill.id, form.body, form.note, operators)
      }
      const [s, v] = await Promise.all([getSkill(skill.id), listVersions(skill.id)])
      setSkill(s)
      setVersions(v)
      setForm((f) => ({ ...f, note: '' }))
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to publish')
    } finally {
      setSaving(false)
    }
  }

  async function onFork() {
    if (!skill) return
    const name = window.prompt(
      `Fork ${skill.name}?\n\nA fork is yours entirely. Nothing the operator does to ` +
        `theirs afterwards reaches it, and nothing merges back -- you keep only a note ` +
        `of where it came from, so you can see what has changed since.\n\nName for your copy:`,
      `${skill.name} (ours)`,
    )
    if (!name) return
    try {
      const made = await forkSkill(skill.id, { slug: slugify(name), name })
      void navigate(`/skills/${made.id}`)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to fork')
    }
  }

  async function onRetire() {
    if (!skill) return
    const retiring = skill.retired_at === null
    if (retiring && !window.confirm(`Retire ${skill.name}? Agents already given it keep it.`)) {
      return
    }
    try {
      setSkill(await retireSkill(skill.id, retiring, operators))
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to retire')
    }
  }

  /** Loads an old body into the draft rather than publishing behind the
   *  reader's back: a rollback is a version like any other, and this is the
   *  moment where that is worth showing rather than explaining. */
  function onRestore(v: SkillVersion) {
    setForm((f) => ({ ...f, body: v.body, note: `rolled back to v${v.ordinal}` }))
  }

  const title = creating ? (overriding ? 'New override' : 'New skill') : loading ? 'Skill' : form.name

  return (
    <div className="p-6 max-w-3xl">
      <Link
        to="/skills"
        className="inline-flex items-center gap-1 text-sm text-surface-600 dark:text-surface-400 hover:underline underline-offset-2"
      >
        <ArrowLeft size={14} aria-hidden />
        Skills
      </Link>

      <h1 className="mt-2 text-2xl font-semibold text-surface-900 dark:text-surface-100">
        {title}
      </h1>

      {base && (
        <p className="mt-2 flex items-center gap-1.5 text-sm text-surface-600 dark:text-surface-400">
          <Layers size={14} aria-hidden />
          {creating ? 'Varies' : 'Varies'} <Link to={`/skills/${base.id}`} className="underline underline-offset-2">{base.name}</Link>{' '}
          wherever it is used. Your instructions are composed after it, so where they
          conflict, yours are the ones followed.
        </p>
      )}

      {operators && !isOperator && (
        <p className="mt-2 text-sm text-surface-600 dark:text-surface-400">
          The operator maintains this skill for every workspace. You cannot edit it here
          — vary it with an override, or take a copy of your own.
        </p>
      )}

      {skill?.base_moved && (
        <p
          className="mt-4 flex items-start gap-2 rounded-md border border-amber-300 dark:border-amber-800 bg-amber-50 dark:bg-amber-950/40 px-3 py-2 text-sm text-amber-800 dark:text-amber-400"
          role="status"
        >
          <AlertTriangle size={16} className="mt-0.5 shrink-0" aria-hidden />
          <span>
            {base?.name ?? 'The skill this varies'} has been edited since these instructions
            were written against it. Nothing has broken — but they may now be correcting
            something that is no longer there. Publishing again records them against the
            current version.
          </span>
        </p>
      )}

      {error && (
        <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      {loading ? (
        <p className="mt-6 text-sm text-surface-600 dark:text-surface-400">Loading…</p>
      ) : (
        <form onSubmit={creating ? onCreate : onPublish} className="mt-6 space-y-4">
          <div className="grid grid-cols-2 gap-4">
            <label className="block">
              <span className="text-sm font-medium text-surface-800 dark:text-surface-200">Name</span>
              <input
                value={form.name}
                disabled={!editable}
                onChange={(e) =>
                  setForm((f) => ({
                    ...f,
                    name: e.target.value,
                    slug: creating && !slugEdited ? slugify(e.target.value) : f.slug,
                  }))
                }
                className="mt-1 w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 text-sm text-surface-900 dark:text-surface-100 disabled:opacity-60"
              />
            </label>
            <label className="block">
              <span className="text-sm font-medium text-surface-800 dark:text-surface-200">Slug</span>
              <input
                value={form.slug}
                disabled={!creating}
                onChange={(e) => {
                  setSlugEdited(true)
                  setForm((f) => ({ ...f, slug: e.target.value }))
                }}
                className="mt-1 w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 text-sm font-mono text-surface-900 dark:text-surface-100 disabled:opacity-60"
              />
            </label>
          </div>

          <label className="block">
            <span className="text-sm font-medium text-surface-800 dark:text-surface-200">
              Description
            </span>
            <input
              value={form.description}
              disabled={!editable}
              onChange={(e) => setForm((f) => ({ ...f, description: e.target.value }))}
              placeholder="What this is for, for whoever picks it later."
              className="mt-1 w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 text-sm text-surface-900 dark:text-surface-100 disabled:opacity-60"
            />
          </label>

          <label className="block">
            <span className="text-sm font-medium text-surface-800 dark:text-surface-200">
              {overriding || skill?.kind === 'override' ? 'Your instructions' : 'The skill'}
            </span>
            <textarea
              value={form.body}
              disabled={!editable}
              onChange={(e) => setForm((f) => ({ ...f, body: e.target.value }))}
              rows={16}
              className="mt-1 w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 text-sm font-mono text-surface-900 dark:text-surface-100 disabled:opacity-60"
            />
          </label>

          {creating && isOperator && !overriding && (
            <label className="flex items-start gap-2">
              <input
                type="checkbox"
                checked={forEveryone}
                onChange={(e) => setForEveryone(e.target.checked)}
                className="mt-1"
              />
              <span className="text-sm text-surface-800 dark:text-surface-200">
                Ship to every workspace
                <span className="block text-xs text-surface-600 dark:text-surface-400">
                  Yours to maintain. Workspaces can override or fork it, but not edit it.
                </span>
              </span>
            </label>
          )}

          {!creating && editable && (
            <label className="block">
              <span className="text-sm font-medium text-surface-800 dark:text-surface-200">
                What changed
              </span>
              <input
                value={form.note}
                onChange={(e) => setForm((f) => ({ ...f, note: e.target.value }))}
                placeholder="Kept with the version, for whoever reads the history."
                className="mt-1 w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 text-sm text-surface-900 dark:text-surface-100"
              />
            </label>
          )}

          <div className="flex items-center gap-2">
            {editable && (
              <button
                type="submit"
                disabled={saving || !form.name.trim() || !form.slug.trim()}
                className="px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium disabled:opacity-50"
              >
                {saving ? 'Saving…' : creating ? 'Create skill' : 'Publish version'}
              </button>
            )}
            {!creating && operators && !isOperator && canWrite && (
              <>
                <Link
                  to={`/skills/new?override=${skill?.id}`}
                  className="flex items-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium"
                >
                  <Layers size={16} aria-hidden />
                  Write an override
                </Link>
                <button
                  type="button"
                  onClick={() => void onFork()}
                  className="flex items-center gap-2 px-4 py-2 rounded-md border border-surface-300 dark:border-surface-700 text-sm font-medium text-surface-800 dark:text-surface-200 hover:bg-surface-50 dark:hover:bg-surface-800"
                >
                  <GitFork size={16} aria-hidden />
                  Fork
                </button>
              </>
            )}
            {!creating && editable && (
              <button
                type="button"
                onClick={() => void onRetire()}
                className="ml-auto px-3 py-2 rounded-md text-sm text-surface-600 dark:text-surface-400 hover:bg-surface-100 dark:hover:bg-surface-800"
              >
                {skill?.retired_at ? 'Bring back' : 'Retire'}
              </button>
            )}
          </div>
        </form>
      )}

      {!creating && versions.length > 0 && (
        <section className="mt-10">
          <h2 className="flex items-center gap-2 text-lg font-semibold text-surface-900 dark:text-surface-100">
            <History size={18} aria-hidden />
            History
          </h2>
          <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
            Every version is kept, and the newest is the one agents are given. Restoring an
            older one brings its words back into the draft to be published again, so the
            history only ever runs forwards.
          </p>
          <ul className="mt-4 divide-y divide-surface-200 dark:divide-surface-800 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 overflow-hidden">
            {versions.map((v) => (
              <li key={v.id} className="flex items-start gap-4 p-4">
                <div className="flex-1 min-w-0">
                  <p className="text-sm font-medium text-surface-900 dark:text-surface-100">
                    v{v.ordinal}
                    {v.ordinal === skill?.ordinal && (
                      <span className="ml-2 text-xs font-normal px-1.5 py-0.5 rounded bg-brand-50 dark:bg-brand-950 text-brand-800 dark:text-brand-300">
                        live
                      </span>
                    )}
                  </p>
                  {v.note && (
                    <p className="text-xs text-surface-600 dark:text-surface-400">{v.note}</p>
                  )}
                  <p className="text-xs text-surface-500 dark:text-surface-500">
                    {new Date(v.created_at).toLocaleString()}
                  </p>
                </div>
                {editable && v.ordinal !== skill?.ordinal && (
                  <button
                    type="button"
                    onClick={() => onRestore(v)}
                    className="px-3 py-1.5 rounded-md border border-surface-300 dark:border-surface-700 text-xs font-medium text-surface-800 dark:text-surface-200 hover:bg-surface-50 dark:hover:bg-surface-800"
                  >
                    Restore
                  </button>
                )}
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  )
}
