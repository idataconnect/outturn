import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { ArrowLeft, Save } from 'lucide-react'

import { ApiError, api } from '../lib/api'
import { Control, type Effective } from '../components/SettingsCascade'
import {
  AVAILABILITY,
  createTemplate,
  getTemplate,
  publishTemplate,
  updateTemplate,
  type Availability,
  type NewVersion,
  type Template,
} from '../lib/agentTemplates'
import { listSkills, type Skill } from '../lib/skills'
import { paths } from '../lib/paths'
import { useSession } from '../lib/session'

/**
 * The default agent's tools, by the names a template offers from the first
 * round. A name the agent does not have is ignored, so this list going stale
 * costs a checkbox that does nothing, never a turn.
 */
const TOOLS = [
  'read_object',
  'write_object',
  'delete_object',
  'list_objects',
  'describe_image',
  'expand_archive',
  'create_archive',
  'render_pdf',
  'fetch_url',
  'get_current_time',
  'sleep',
  'set_timer',
  'list_timers',
  'cancel_timer',
]

type Form = NewVersion & {
  slug: string
  availability: Availability
  allow_additions: boolean
  model: string
}

const EMPTY: Form = {
  slug: '',
  availability: 'optional',
  allow_additions: true,
  name: '',
  description: '',
  requirements: '',
  defaults: '',
  reminder: '',
  policy: {},
  model: '',
  eager_tools: [],
  skills: [],
  settings: {},
  note: '',
}

function fromTemplate(t: Template): Form {
  const model = typeof t.current.policy.model === 'string' ? t.current.policy.model : ''
  return {
    slug: t.slug,
    availability: t.availability,
    allow_additions: t.allow_additions,
    name: t.current.name,
    description: t.current.description,
    requirements: t.current.requirements,
    defaults: t.current.defaults,
    reminder: t.current.reminder,
    policy: t.current.policy,
    model,
    eager_tools: t.current.eager_tools,
    skills: t.current.skills,
    settings: t.current.settings ?? {},
    // A note says what one publish changed, so it does not carry forward.
    note: '',
  }
}

/** The version a form publishes. The model is a field of its own here and
 *  part of the policy on the wire; the rest of the policy rides along. */
function version(form: Form): NewVersion {
  const policy = { ...form.policy }
  if (form.model.trim()) policy.model = form.model.trim()
  else delete policy.model
  return {
    name: form.name,
    description: form.description,
    requirements: form.requirements,
    defaults: form.defaults,
    reminder: form.reminder,
    policy,
    eager_tools: form.eager_tools,
    skills: form.skills,
    settings: form.settings,
    note: form.note,
  }
}

const field =
  'w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100'
const label = 'block text-sm text-surface-700 dark:text-surface-300 mb-1'
const hint = 'block text-xs text-surface-500 dark:text-surface-400 mb-1'

/**
 * Creating a template, and publishing a new version of one.
 *
 * A version is never edited: saving publishes the whole form as the next one,
 * which every workspace's agent follows from its next turn. Availability, the
 * additions switch and retiring are the template's, not a version's, and are
 * saved with it.
 */
export default function AgentTemplateEditor() {
  const { id } = useParams<{ id?: string }>()
  const creating = id === undefined
  const navigate = useNavigate()
  const state = useSession()
  const workspaceId = state.status === 'authenticated' ? state.session.workspace_id : null

  const [form, setForm] = useState<Form>(EMPTY)
  const [loaded, setLoaded] = useState<Template | null>(null)
  const [skills, setSkills] = useState<Skill[]>([])
  /** The settings catalog, as the platform sees it, for the fixed-settings
   *  controls: each setting's kind, label and the value it has now. */
  const [catalog, setCatalog] = useState<Effective[]>([])
  const [loading, setLoading] = useState(!creating)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let stale = false
    void (async () => {
      try {
        // Only the operator's own skills: a template is in every workspace,
        // and a workspace's skill there would be that workspace's everywhere.
        const [all, settings] = await Promise.all([
          listSkills(),
          api<Effective[]>('/v1/platform/settings'),
        ])
        if (!stale) {
          setSkills(all.filter((s) => s.workspace_id !== workspaceId && !s.retired_at))
          setCatalog(settings)
        }
        if (creating) return
        const t = await getTemplate(id)
        if (stale) return
        setLoaded(t)
        setForm(fromTemplate(t))
      } catch (e) {
        if (!stale) setError(e instanceof ApiError ? e.message : 'failed to load template')
      } finally {
        if (!stale) setLoading(false)
      }
    })()
    return () => {
      stale = true
    }
  }, [id, creating, workspaceId])

  const complete = form.name.trim() !== '' && (!creating || form.slug.trim() !== '')

  function toggle<T>(list: T[], item: T, on: boolean): T[] {
    return on ? [...list, item] : list.filter((x) => x !== item)
  }

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault()
    setSaving(true)
    setError(null)
    try {
      if (creating) {
        const made = await createTemplate({
          ...version(form),
          slug: form.slug,
          availability: form.availability,
          allow_additions: form.allow_additions,
        })
        void navigate(paths.agentTemplate(made.id))
        return
      }
      if (
        loaded &&
        (loaded.availability !== form.availability ||
          loaded.allow_additions !== form.allow_additions)
      ) {
        await updateTemplate(id, {
          availability: form.availability,
          allow_additions: form.allow_additions,
        })
      }
      const published = await publishTemplate(id, version(form))
      setLoaded(published)
      setForm(fromTemplate(published))
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to save template')
    } finally {
      setSaving(false)
    }
  }

  async function onRetire(retired: boolean) {
    if (!id) return
    try {
      const t = await updateTemplate(id, { retired })
      setLoaded(t)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to save template')
    }
  }

  return (
    <div className="p-6 max-w-3xl">
      <Link
        to={paths.agentTemplates}
        className="inline-flex items-center gap-1 text-sm text-surface-600 dark:text-surface-400 hover:text-surface-900 dark:hover:text-surface-100"
      >
        <ArrowLeft size={14} aria-hidden />
        Agent templates
      </Link>
      <h1 className="mt-2 text-2xl font-semibold text-surface-900 dark:text-surface-100">
        {creating ? 'New template' : loading ? 'Template' : form.name}
      </h1>
      {loaded && (
        <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
          Version {loaded.current.ordinal}
          {loaded.retired && ', retired: no longer offered or made in new workspaces'}
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
        <form
          onSubmit={onSubmit}
          className="mt-6 space-y-5 p-4 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900"
        >
          <div className="flex flex-wrap items-end gap-3">
            <label className="flex-1 min-w-44">
              <span className={label}>Name</span>
              <input
                value={form.name}
                onChange={(e) => setForm({ ...form, name: e.target.value })}
                required
                className={field}
              />
            </label>
            <label className="flex-1 min-w-44">
              <span className={label}>Slug</span>
              <input
                value={form.slug}
                onChange={(e) => setForm({ ...form, slug: e.target.value })}
                required
                pattern="[a-z0-9\-]+"
                disabled={!creating}
                className={`${field} font-mono text-sm disabled:opacity-60`}
              />
            </label>
          </div>

          <label className="block">
            <span className={label}>Description</span>
            <input
              value={form.description}
              onChange={(e) => setForm({ ...form, description: e.target.value })}
              className={field}
            />
          </label>

          <fieldset>
            <legend className={label}>Availability</legend>
            <div className="space-y-1">
              {(Object.keys(AVAILABILITY) as Availability[]).map((a) => (
                <label key={a} className="flex items-start gap-2">
                  <input
                    type="radio"
                    name="availability"
                    checked={form.availability === a}
                    onChange={() => setForm({ ...form, availability: a })}
                    className="mt-1"
                  />
                  <span className="text-sm text-surface-700 dark:text-surface-300">
                    {AVAILABILITY[a].label}
                    <span className="block text-xs text-surface-500 dark:text-surface-400">
                      {AVAILABILITY[a].detail}
                    </span>
                  </span>
                </label>
              ))}
            </div>
          </fieldset>

          <label className="block">
            <span className={label}>Requirements</span>
            <span className={hint}>What every workspace&rsquo;s agent must do. No workspace changes these.</span>
            <textarea
              value={form.requirements}
              onChange={(e) => setForm({ ...form, requirements: e.target.value })}
              rows={6}
              className={field}
            />
          </label>

          <label className="block">
            <span className={label}>Defaults</span>
            <span className={hint}>
              How most businesses work. A workspace may describe its own way in its section.
            </span>
            <textarea
              value={form.defaults}
              onChange={(e) => setForm({ ...form, defaults: e.target.value })}
              rows={6}
              className={field}
            />
          </label>

          <label className="block">
            <span className={label}>Reminder</span>
            <span className={hint}>
              The requirements restated in a line, read last. Optional.
            </span>
            <input
              value={form.reminder}
              onChange={(e) => setForm({ ...form, reminder: e.target.value })}
              className={field}
            />
          </label>

          <label className="flex items-start gap-2">
            <input
              type="checkbox"
              checked={form.allow_additions}
              onChange={(e) => setForm({ ...form, allow_additions: e.target.checked })}
              className="mt-1"
            />
            <span className="text-sm text-surface-700 dark:text-surface-300">
              Workspaces may describe how their business works
              <span className="block text-xs text-surface-500 dark:text-surface-400">
                Off keeps the instructions exactly as written in every workspace.
              </span>
            </span>
          </label>

          <label className="block">
            <span className={label}>Model</span>
            <span className={hint}>The model this agent&rsquo;s turns ask for.</span>
            <input
              value={form.model}
              onChange={(e) => setForm({ ...form, model: e.target.value })}
              className={`${field} font-mono text-sm`}
            />
          </label>

          <fieldset>
            <legend className={label}>Skills</legend>
            <span className={hint}>The operator&rsquo;s skills, given to every agent made from this.</span>
            {skills.length === 0 ? (
              <p className="text-sm text-surface-500 dark:text-surface-400">No platform skills yet.</p>
            ) : (
              <div className="space-y-1">
                {skills.map((s) => (
                  <label key={s.id} className="flex items-center gap-2 text-sm text-surface-700 dark:text-surface-300">
                    <input
                      type="checkbox"
                      checked={form.skills.some((b) => b.skill_id === s.id)}
                      onChange={(e) =>
                        setForm({
                          ...form,
                          skills: e.target.checked
                            ? [...form.skills, { skill_id: s.id }]
                            : form.skills.filter((b) => b.skill_id !== s.id),
                        })
                      }
                    />
                    {s.name}
                  </label>
                ))}
              </div>
            )}
          </fieldset>

          <fieldset>
            <legend className={label}>Tools offered from the first round</legend>
            <span className={hint}>
              The tools this agent reaches for on most turns. The rest are loaded when asked for.
            </span>
            <div className="grid grid-cols-2 gap-1">
              {TOOLS.map((tool) => (
                <label key={tool} className="flex items-center gap-2 text-sm font-mono text-surface-700 dark:text-surface-300">
                  <input
                    type="checkbox"
                    checked={form.eager_tools.includes(tool)}
                    onChange={(e) =>
                      setForm({ ...form, eager_tools: toggle(form.eager_tools, tool, e.target.checked) })
                    }
                  />
                  {tool}
                </label>
              ))}
            </div>
          </fieldset>

          <fieldset>
            <legend className={label}>Fixed settings</legend>
            <span className={hint}>
              Values every agent made from this runs with, whatever its workspace sets.
            </span>
            <div className="space-y-2">
              {catalog.map((setting) => {
                const fixed = setting.key in form.settings
                return (
                  <div key={setting.key} className="flex flex-wrap items-center gap-3">
                    <label className="flex items-center gap-2 w-56 text-sm text-surface-700 dark:text-surface-300">
                      <input
                        type="checkbox"
                        checked={fixed}
                        onChange={(e) => {
                          const settings = { ...form.settings }
                          // Fixing starts from the value that applies now,
                          // so nothing changes until it is edited.
                          if (e.target.checked) settings[setting.key] = setting.value
                          else delete settings[setting.key]
                          setForm({ ...form, settings })
                        }}
                      />
                      {setting.label}
                    </label>
                    <Control
                      setting={{ ...setting, value: fixed ? form.settings[setting.key] : setting.value }}
                      disabled={!fixed}
                      onChange={(value) =>
                        setForm({ ...form, settings: { ...form.settings, [setting.key]: value } })
                      }
                    />
                  </div>
                )
              })}
            </div>
          </fieldset>

          {!creating && (
            <label className="block">
              <span className={label}>What changed</span>
              <span className={hint}>Kept with this version, for whoever reads its history.</span>
              <input
                value={form.note}
                onChange={(e) => setForm({ ...form, note: e.target.value })}
                className={field}
              />
            </label>
          )}

          <div className="flex items-center gap-3">
            <button
              type="submit"
              disabled={saving || !complete || loaded?.retired}
              className="flex items-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium disabled:opacity-50"
            >
              <Save size={16} aria-hidden />
              {saving ? 'Saving…' : creating ? 'Create template' : 'Publish new version'}
            </button>
            {loaded && (
              <button
                type="button"
                onClick={() => void onRetire(!loaded.retired)}
                className="text-sm text-surface-600 dark:text-surface-400 hover:text-surface-900 dark:hover:text-surface-100"
              >
                {loaded.retired ? 'Bring back' : 'Retire'}
              </button>
            )}
          </div>
        </form>
      )}
    </div>
  )
}
