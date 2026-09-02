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

export default function Agents() {
  const state = useSession()
  const [agents, setAgents] = useState<Agent[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [form, setForm] = useState({ name: '', slug: '', system_prompt: '' })
  const [slugEdited, setSlugEdited] = useState(false)
  const [creating, setCreating] = useState(false)

  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canCreate = authorities.includes('agents:create')
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
      await api<Agent>('/v1/agents', { method: 'POST', body: JSON.stringify(form) })
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
      <h1 className="text-2xl font-semibold text-gray-900 dark:text-gray-100">Agents</h1>
      <p className="mt-2 text-gray-600 dark:text-gray-400">
        Agents belong to the tenant you are currently viewing.
      </p>

      {canCreate && (
        <form
          onSubmit={onCreate}
          className="mt-6 space-y-3 p-4 rounded-lg border border-gray-200 dark:border-gray-800 bg-white dark:bg-gray-900"
        >
          <div className="flex flex-wrap items-end gap-3">
            <label className="flex-1 min-w-44">
              <span className="block text-sm text-gray-700 dark:text-gray-300 mb-1">Name</span>
              <input
                value={form.name}
                onChange={(e) => onNameChange(e.target.value)}
                required
                className="w-full px-3 py-2 rounded-md border border-gray-300 dark:border-gray-700 bg-white dark:bg-gray-950 text-gray-900 dark:text-gray-100"
              />
            </label>
            <label className="flex-1 min-w-44">
              <span className="block text-sm text-gray-700 dark:text-gray-300 mb-1">Slug</span>
              <input
                value={form.slug}
                onChange={(e) => {
                  setSlugEdited(true)
                  setForm({ ...form, slug: e.target.value })
                }}
                required
                pattern="[a-z0-9\-]+"
                className="w-full px-3 py-2 rounded-md border border-gray-300 dark:border-gray-700 bg-white dark:bg-gray-950 text-gray-900 dark:text-gray-100 font-mono text-sm"
              />
            </label>
          </div>
          <label className="block">
            <span className="block text-sm text-gray-700 dark:text-gray-300 mb-1">
              System prompt
            </span>
            <textarea
              value={form.system_prompt}
              onChange={(e) => setForm({ ...form, system_prompt: e.target.value })}
              rows={3}
              className="w-full px-3 py-2 rounded-md border border-gray-300 dark:border-gray-700 bg-white dark:bg-gray-950 text-gray-900 dark:text-gray-100"
            />
          </label>
          <button
            type="submit"
            disabled={creating}
            className="flex items-center gap-2 px-4 py-2 rounded-md bg-gray-900 dark:bg-gray-100 text-white dark:text-gray-900 text-sm font-medium disabled:opacity-50"
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

      <div className="mt-6 rounded-lg border border-gray-200 dark:border-gray-800 bg-white dark:bg-gray-900 overflow-hidden">
        {loading ? (
          <p className="p-4 text-sm text-gray-500 dark:text-gray-400">Loading…</p>
        ) : agents.length === 0 ? (
          <p className="p-4 text-sm text-gray-500 dark:text-gray-400">No agents yet.</p>
        ) : (
          <ul className="divide-y divide-gray-200 dark:divide-gray-800">
            {agents.map((agent) => (
              <li key={agent.id} className="flex items-start gap-4 p-4">
                <Bot size={16} className="mt-1 shrink-0 text-gray-400" />
                <div className="flex-1 min-w-0">
                  <p className="text-sm font-medium text-gray-900 dark:text-gray-100 truncate">
                    {agent.name}
                    {!agent.enabled && (
                      <span className="ml-2 text-xs text-gray-500 dark:text-gray-400">
                        disabled
                      </span>
                    )}
                  </p>
                  <p className="text-xs font-mono text-gray-500 dark:text-gray-400 truncate">
                    {agent.slug}
                  </p>
                  {agent.system_prompt && (
                    <p className="mt-1 text-xs text-gray-600 dark:text-gray-400 line-clamp-2">
                      {agent.system_prompt}
                    </p>
                  )}
                </div>
                {canDelete && (
                  <button
                    onClick={() => void onDelete(agent.id)}
                    aria-label={`Delete ${agent.name}`}
                    className="p-2 rounded-md text-gray-400 hover:text-red-600 dark:hover:text-red-400 hover:bg-gray-100 dark:hover:bg-gray-800"
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
