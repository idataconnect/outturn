import { useEffect, useState } from 'react'
import { Plus, Trash2 } from 'lucide-react'

import { ApiError, api } from '../lib/api'

type Tenant = {
  id: string
  name: string
  slug: string
}

export default function Tenants() {
  const [tenants, setTenants] = useState<Tenant[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [name, setName] = useState('')
  const [slug, setSlug] = useState('')
  const [slugEdited, setSlugEdited] = useState(false)
  const [creating, setCreating] = useState(false)

  async function refresh() {
    try {
      setTenants(await api<Tenant[]>('/v1/tenants'))
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to load tenants')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    void refresh()
  }, [])

  function onNameChange(value: string) {
    setName(value)
    if (!slugEdited) {
      setSlug(
        value
          .toLowerCase()
          .replace(/[^a-z0-9]+/g, '-')
          .replace(/^-+|-+$/g, ''),
      )
    }
  }

  async function onCreate(event: React.FormEvent) {
    event.preventDefault()
    setCreating(true)
    try {
      await api<Tenant>('/v1/tenants', {
        method: 'POST',
        body: JSON.stringify({ name, slug }),
      })
      setName('')
      setSlug('')
      setSlugEdited(false)
      setError(null)
      await refresh()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to create tenant')
    } finally {
      setCreating(false)
    }
  }

  async function onDelete(id: string) {
    try {
      await api<void>(`/v1/tenants/${id}`, { method: 'DELETE' })
      await refresh()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to delete tenant')
    }
  }

  return (
    <div className="p-6 max-w-3xl">
      <h1 className="text-2xl font-semibold text-gray-900 dark:text-gray-100">Tenants</h1>
      <p className="mt-2 text-gray-600 dark:text-gray-400">
        Create and remove tenants. Visible to system administrators only.
      </p>

      <form
        onSubmit={onCreate}
        className="mt-6 flex flex-wrap items-end gap-3 p-4 rounded-lg border border-gray-200 dark:border-gray-800 bg-white dark:bg-gray-900"
      >
        <label className="flex-1 min-w-48">
          <span className="block text-sm text-gray-700 dark:text-gray-300 mb-1">Name</span>
          <input
            value={name}
            onChange={(e) => onNameChange(e.target.value)}
            required
            className="w-full px-3 py-2 rounded-md border border-gray-300 dark:border-gray-700 bg-white dark:bg-gray-950 text-gray-900 dark:text-gray-100"
          />
        </label>
        <label className="flex-1 min-w-48">
          <span className="block text-sm text-gray-700 dark:text-gray-300 mb-1">Slug</span>
          <input
            value={slug}
            onChange={(e) => {
              setSlugEdited(true)
              setSlug(e.target.value)
            }}
            required
            pattern="[a-z0-9\-]+"
            className="w-full px-3 py-2 rounded-md border border-gray-300 dark:border-gray-700 bg-white dark:bg-gray-950 text-gray-900 dark:text-gray-100 font-mono text-sm"
          />
        </label>
        <button
          type="submit"
          disabled={creating}
          className="flex items-center gap-2 px-4 py-2 rounded-md bg-gray-900 dark:bg-gray-100 text-white dark:text-gray-900 text-sm font-medium disabled:opacity-50"
        >
          <Plus size={16} />
          {creating ? 'Creating…' : 'Create'}
        </button>
      </form>

      {error && (
        <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      <div className="mt-6 rounded-lg border border-gray-200 dark:border-gray-800 bg-white dark:bg-gray-900 overflow-hidden">
        {loading ? (
          <p className="p-4 text-sm text-gray-500 dark:text-gray-400">Loading…</p>
        ) : tenants.length === 0 ? (
          <p className="p-4 text-sm text-gray-500 dark:text-gray-400">No tenants yet.</p>
        ) : (
          <ul className="divide-y divide-gray-200 dark:divide-gray-800">
            {tenants.map((tenant) => (
              <li key={tenant.id} className="flex items-center gap-4 p-4">
                <div className="flex-1 min-w-0">
                  <p className="text-sm font-medium text-gray-900 dark:text-gray-100 truncate">
                    {tenant.name}
                  </p>
                  <p className="text-xs font-mono text-gray-500 dark:text-gray-400 truncate">
                    {tenant.slug}
                  </p>
                </div>
                <button
                  onClick={() => void onDelete(tenant.id)}
                  aria-label={`Delete ${tenant.name}`}
                  className="p-2 rounded-md text-gray-400 hover:text-red-600 dark:hover:text-red-400 hover:bg-gray-100 dark:hover:bg-gray-800"
                >
                  <Trash2 size={16} />
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  )
}
