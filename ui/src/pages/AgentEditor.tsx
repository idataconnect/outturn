import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { ArrowLeft, Save } from 'lucide-react'

import { ApiError, api } from '../lib/api'
import { useSession } from '../lib/session'
import SettingsCascade from '../components/SettingsCascade'
import AgentSkills from '../components/AgentSkills'

export type Agent = {
  id: string
  name: string
  slug: string
  description: string
  system_prompt: string
  policy: { reasoning_effort?: string; [key: string]: unknown }
  enabled: boolean
}

/** What the form holds, whichever way it was reached. */
type Form = {
  name: string
  slug: string
  description: string
  system_prompt: string
  enabled: boolean
}

const EMPTY: Form = {
  name: '',
  slug: '',
  description: '',
  system_prompt: '',
  enabled: true,
}

function fromAgent(agent: Agent): Form {
  return {
    name: agent.name,
    slug: agent.slug,
    description: agent.description,
    system_prompt: agent.system_prompt,
    enabled: agent.enabled,
  }
}

function slugify(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
}

const field =
  'w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100'
const label = 'block text-sm text-surface-700 dark:text-surface-300 mb-1'

/**
 * One page for creating an agent and for changing one.
 *
 * `/agents/new` starts empty; `/agents/:id` loads the agent first. The slug
 * is settled at creation -- it is how sessions and URLs name the agent -- so
 * it is shown but not editable afterwards. Everything else can change.
 */
export default function AgentEditor() {
  const { id } = useParams<{ id?: string }>()
  const creating = id === undefined
  const navigate = useNavigate()
  const state = useSession()
  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canSave = authorities.includes(creating ? 'agents:create' : 'agents:update')

  const [form, setForm] = useState<Form>(EMPTY)
  const [slugEdited, setSlugEdited] = useState(false)
  const [loading, setLoading] = useState(!creating)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (creating) return
    let stale = false
    void (async () => {
      try {
        const agent = await api<Agent>(`/v1/agents/${id}`)
        if (!stale) setForm(fromAgent(agent))
      } catch (e) {
        if (!stale) setError(e instanceof ApiError ? e.message : 'failed to load agent')
      } finally {
        if (!stale) setLoading(false)
      }
    })()
    return () => {
      stale = true
    }
  }, [id, creating])

  // The same fields the browser refuses to submit without, reflected on the
  // button so the form says what it needs before it is clicked -- trimmed,
  // because a name of spaces is not a name.
  const complete = form.name.trim() !== '' && form.slug.trim() !== ''

  function onNameChange(value: string) {
    setForm((f) => ({
      ...f,
      name: value,
      slug: creating && !slugEdited ? slugify(value) : f.slug,
    }))
  }

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault()
    setSaving(true)
    try {
      if (creating) {
        const made = await api<Agent>('/v1/agents', {
          method: 'POST',
          body: JSON.stringify({
            name: form.name,
            slug: form.slug,
            description: form.description,
            system_prompt: form.system_prompt,
          }),
        })
        // Straight to the editor, where the behavior settings live: they
        // are overrides on an agent that has to exist first. Navigating here
        // does not unmount this component -- both routes render AgentEditor
        // in the same spot in the tree -- so saving must be cleared by hand.
        setSaving(false)
        void navigate(`/agents/${made.id}`)
        return
      }
      await api<Agent>(`/v1/agents/${id}`, {
        method: 'PATCH',
        body: JSON.stringify({
          name: form.name,
          description: form.description,
          system_prompt: form.system_prompt,
          enabled: form.enabled,
        }),
      })
      setSaving(false)
      void navigate('/agents')
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to save agent')
      setSaving(false)
    }
  }

  return (
    <div className="p-6 max-w-3xl">
      <Link
        to="/agents"
        className="inline-flex items-center gap-1 text-sm text-surface-600 dark:text-surface-400 hover:text-surface-900 dark:hover:text-surface-100"
      >
        <ArrowLeft size={14} aria-hidden />
        Agents
      </Link>
      <h1 className="mt-2 text-2xl font-semibold text-surface-900 dark:text-surface-100">
        {creating ? 'New agent' : loading ? 'Agent' : form.name}
      </h1>

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
          className="mt-6 space-y-4 p-4 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900"
        >
          <div className="flex flex-wrap items-end gap-3">
            <label className="flex-1 min-w-44">
              <span className={label}>Name</span>
              <input
                value={form.name}
                onChange={(e) => onNameChange(e.target.value)}
                required
                disabled={!canSave}
                className={field}
              />
            </label>
            <label className="flex-1 min-w-44">
              <span className={label}>Slug</span>
              <input
                value={form.slug}
                onChange={(e) => {
                  setSlugEdited(true)
                  setForm({ ...form, slug: e.target.value })
                }}
                required
                pattern="[a-z0-9\-]+"
                // Fixed once created: sessions and URLs already name the
                // agent by it.
                disabled={!creating || !canSave}
                className={`${field} font-mono text-sm disabled:opacity-60`}
              />
            </label>
          </div>

          <label className="block">
            <span className={label}>Description</span>
            <input
              value={form.description}
              onChange={(e) => setForm({ ...form, description: e.target.value })}
              disabled={!canSave}
              className={field}
            />
          </label>

          <label className="block">
            <span className={label}>System prompt</span>
            <textarea
              value={form.system_prompt}
              onChange={(e) => setForm({ ...form, system_prompt: e.target.value })}
              rows={8}
              disabled={!canSave}
              className={field}
            />
          </label>

          {!creating && (
            <label className="flex items-start gap-2">
              <input
                type="checkbox"
                checked={form.enabled}
                onChange={(e) => setForm({ ...form, enabled: e.target.checked })}
                disabled={!canSave}
                className="mt-1"
              />
              <span className="text-sm text-surface-700 dark:text-surface-300">
                Enabled
                <span className="block text-xs text-surface-500 dark:text-surface-400">
                  A disabled agent keeps its sessions but answers nothing new.
                </span>
              </span>
            </label>
          )}

          {canSave && (
            <div className="flex items-center gap-3">
              <button
                type="submit"
                disabled={saving || !complete}
                className="flex items-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium disabled:opacity-50"
              >
                <Save size={16} aria-hidden />
                {saving ? 'Saving…' : creating ? 'Create agent' : 'Save changes'}
              </button>
              <Link
                to="/agents"
                className="text-sm text-surface-600 dark:text-surface-400 hover:text-surface-900 dark:hover:text-surface-100"
              >
                Cancel
              </Link>
            </div>
          )}
        </form>
      )}

      {!creating && !loading && id && <AgentSkills agentId={id} />}

      {!creating && !loading && (
        <section className="mt-8 space-y-4">
          <div>
            <h2 className="text-lg font-semibold text-surface-900 dark:text-surface-100">
              Behavior
            </h2>
            <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
              Each value comes from the workspace unless overridden for this agent.
            </p>
          </div>
          <SettingsCascade
            base={`/v1/agents/${id}/settings`}
            canEdit={authorities.includes('settings:update')}
            levelName="this agent"
          />
        </section>
      )}
    </div>
  )
}
