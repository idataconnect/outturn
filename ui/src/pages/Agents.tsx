import { useEffect, useState } from 'react'
import { Bot, Plus, Trash2 } from 'lucide-react'

import { ApiError, api } from '../lib/api'
import { useSession } from '../lib/session'

type Agent = {
  id: string
  name: string
  slug: string
  description: string
  system_prompt: string
  enabled: boolean
}

/// The values a provider understands, in the order they cost.
const EFFORTS = ['low', 'medium', 'high'] as const
type Effort = (typeof EFFORTS)[number]

export default function Agents() {
  const state = useSession()
  const [agents, setAgents] = useState<Agent[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [form, setForm] = useState({ name: '', slug: '', system_prompt: '' })
  // Thinking is on by default wherever a provider supports it, and on a local
  // model it costs far more than the answer: a reply of a few dozen characters
  // has been measured spending three hundred tokens deliberating first, which
  // is half a minute before anything appears. Off unless someone asks for it.
  const [deliberate, setDeliberate] = useState(false)
  // The values a provider understands, in the order they cost. Kept as the
  // wire values rather than mapped from prettier ones, so what is stored is
  // what was chosen.
  const [effort, setEffort] = useState<Effort>('medium')
  const [slugEdited, setSlugEdited] = useState(false)
  const [creating, setCreating] = useState(false)

  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canCreate = authorities.includes('agents:create')
  // Only worth saying to somebody who can be looking at more than one tenant.
  // To everybody else there is no "currently viewing" -- there is only their
  // workspace -- and the sentence raises a question they cannot act on.
  const manyTenants =
    state.status === 'authenticated' && state.session.tenants.length > 1
  const canDelete = authorities.includes('agents:delete')

  async function refresh() {
    try {
      setAgents(await api<Agent[]>('/v1/agents'))
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to load agents')
    } finally {
      setLoading(false)
    }
  }

  // Agents are scoped to the active tenant, so switching reloads the list.
  const tenantId = state.status === 'authenticated' ? state.session.tenant_id : null
  useEffect(() => {
    void refresh()
  }, [tenantId])

  function onNameChange(value: string) {
    setForm((f) => ({
      ...f,
      name: value,
      slug: slugEdited
        ? f.slug
        : value
            .toLowerCase()
            .replace(/[^a-z0-9]+/g, '-')
            .replace(/^-+|-+$/g, ''),
    }))
  }

  async function onCreate(event: React.FormEvent) {
    event.preventDefault()
    setCreating(true)
    try {
      await api<Agent>('/v1/agents', {
        method: 'POST',
        body: JSON.stringify({
          ...form,
          policy: { reasoning_effort: deliberate ? effort : 'none' },
        }),
      })
      setForm({ name: '', slug: '', system_prompt: '' })
      setSlugEdited(false)
      setError(null)
      await refresh()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to create agent')
    } finally {
      setCreating(false)
    }
  }

  async function onDelete(id: string) {
    try {
      await api<void>(`/v1/agents/${id}`, { method: 'DELETE' })
      await refresh()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to delete agent')
    }
  }

  return (
    <div className="p-6 max-w-3xl">
      <h1 className="text-2xl font-semibold text-surface-900 dark:text-surface-100">Agents</h1>
      {manyTenants && (
        <p className="mt-2 text-surface-600 dark:text-surface-400">
          Agents belong to the tenant you are currently viewing.
        </p>
      )}

      {canCreate && (
        <form
          onSubmit={onCreate}
          className="mt-6 space-y-3 p-4 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900"
        >
          <div className="flex flex-wrap items-end gap-3">
            <label className="flex-1 min-w-44">
              <span className="block text-sm text-surface-700 dark:text-surface-300 mb-1">Name</span>
              <input
                value={form.name}
                onChange={(e) => onNameChange(e.target.value)}
                required
                className="w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100"
              />
            </label>
            <label className="flex-1 min-w-44">
              <span className="block text-sm text-surface-700 dark:text-surface-300 mb-1">Slug</span>
              <input
                value={form.slug}
                onChange={(e) => {
                  setSlugEdited(true)
                  setForm({ ...form, slug: e.target.value })
                }}
                required
                pattern="[a-z0-9\-]+"
                className="w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100 font-mono text-sm"
              />
            </label>
          </div>
          <label className="block">
            <span className="block text-sm text-surface-700 dark:text-surface-300 mb-1">
              System prompt
            </span>
            <textarea
              value={form.system_prompt}
              onChange={(e) => setForm({ ...form, system_prompt: e.target.value })}
              rows={3}
              className="w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100"
            />
          </label>
          <div className="space-y-2">
            <label className="flex items-start gap-2">
              <input
                type="checkbox"
                checked={deliberate}
                onChange={(e) => setDeliberate(e.target.checked)}
                className="mt-1"
              />
              <span className="text-sm text-surface-700 dark:text-surface-300">
                Let the model think before answering
                <span className="block text-xs text-surface-500 dark:text-surface-400">
                  Better on hard questions, and slower to reply.
                </span>
              </span>
            </label>

            {/* Shown only when it applies. A disabled slider beside an
                unticked box invites someone to set it and wonder why nothing
                changed. */}
            {deliberate && (
              <label className="block pl-6">
                <span className="flex items-baseline justify-between text-sm text-surface-700 dark:text-surface-300 mb-1">
                  <span>How much thinking</span>
                  <span
                    aria-hidden
                    className="text-xs font-medium text-surface-500 dark:text-surface-400 capitalize"
                  >
                    {effort}
                  </span>
                </span>
                <input
                  type="range"
                  min={0}
                  max={2}
                  step={1}
                  value={EFFORTS.indexOf(effort)}
                  onChange={(e) => setEffort(EFFORTS[Number(e.target.value)])}
                  // A range announces its number, so without this it reads as
                  // "1 of 3" -- a position on a scale nobody described. The
                  // text says what the number means, and matches what is shown.
                  aria-valuetext={effort}
                  className="w-full accent-brand-600"
                />
                <span aria-hidden className="flex justify-between text-xs text-surface-500 dark:text-surface-400">
                  <span>Low</span>
                  <span>High</span>
                </span>
              </label>
            )}
          </div>
          <button
            type="submit"
            disabled={creating}
            className="flex items-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium disabled:opacity-50"
          >
            <Plus size={16} />
            {creating ? 'Creating…' : 'Create agent'}
          </button>
        </form>
      )}

      {error && (
        <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      <div className="mt-6 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 overflow-hidden">
        {loading ? (
          <p className="p-4 text-sm text-surface-600 dark:text-surface-400">Loading…</p>
        ) : agents.length === 0 ? (
          <p className="p-4 text-sm text-surface-600 dark:text-surface-400">No agents yet.</p>
        ) : (
          <ul className="divide-y divide-surface-200 dark:divide-surface-800">
            {agents.map((agent) => (
              <li key={agent.id} className="flex items-start gap-4 p-4">
                <Bot size={16} className="mt-1 shrink-0 text-surface-400" />
                <div className="flex-1 min-w-0">
                  <p className="text-sm font-medium text-surface-900 dark:text-surface-100 truncate">
                    {agent.name}
                    {!agent.enabled && (
                      <span className="ml-2 text-xs text-surface-600 dark:text-surface-400">
                        disabled
                      </span>
                    )}
                  </p>
                  <p className="text-xs font-mono text-surface-600 dark:text-surface-400 truncate">
                    {agent.slug}
                  </p>
                  {agent.system_prompt && (
                    <p className="mt-1 text-xs text-surface-600 dark:text-surface-400 line-clamp-2">
                      {agent.system_prompt}
                    </p>
                  )}
                </div>
                {canDelete && (
                  <button
                    onClick={() => void onDelete(agent.id)}
                    aria-label={`Delete ${agent.name}`}
                    className="p-2 rounded-md text-surface-400 hover:text-red-600 dark:hover:text-red-400 hover:bg-surface-100 dark:hover:bg-surface-800"
                  >
                    <Trash2 size={16} />
                  </button>
                )}
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  )
}
