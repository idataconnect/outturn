import { useEffect, useState } from 'react'
import { Plus, Shield, Trash2 } from 'lucide-react'

import { ApiError, api } from '../lib/api'
import { useSession } from '../lib/session'

type Identity = {
  id: string
  provider: string
  subject: string
  verified: boolean
}

type User = {
  id: string
  display_name: string
  identities: Identity[]
  system_roles: string[]
}

const TENANT_ROLES = ['admin', 'operator', 'viewer'] as const

export default function Users() {
  const state = useSession()
  const [users, setUsers] = useState<User[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [form, setForm] = useState({ email: '', display_name: '', password: '' })
  const [role, setRole] = useState<string>('viewer')
  const [creating, setCreating] = useState(false)

  const tenantId = state.status === 'authenticated' ? state.session.tenant_id : null

  async function refresh() {
    try {
      setUsers(await api<User[]>('/v1/users'))
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to load users')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    void refresh()
  }, [])

  async function onCreate(event: React.FormEvent) {
    event.preventDefault()
    if (!tenantId) return
    setCreating(true)
    try {
      const user = await api<User>('/v1/users', {
        method: 'POST',
        body: JSON.stringify(form),
      })
      // Grant the chosen role in the tenant the admin is currently scoped to.
      await api<void>(`/v1/users/${user.id}/tenants/${tenantId}/roles`, {
        method: 'POST',
        body: JSON.stringify({ role }),
      })
      setForm({ email: '', display_name: '', password: '' })
      setError(null)
      await refresh()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to create user')
    } finally {
      setCreating(false)
    }
  }

  async function onDelete(id: string) {
    try {
      await api<void>(`/v1/users/${id}`, { method: 'DELETE' })
      await refresh()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to delete user')
    }
  }

  return (
    <div className="p-6 max-w-3xl">
      <h1 className="text-2xl font-semibold text-surface-900 dark:text-surface-100">Users</h1>
      <p className="mt-2 text-surface-600 dark:text-surface-400">
        New users are granted their role in the tenant you are currently viewing.
      </p>

      <form
        onSubmit={onCreate}
        className="mt-6 flex flex-wrap items-end gap-3 p-4 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900"
      >
        <label className="flex-1 min-w-44">
          <span className="block text-sm text-surface-700 dark:text-surface-300 mb-1">Email</span>
          <input
            type="email"
            value={form.email}
            onChange={(e) => setForm({ ...form, email: e.target.value })}
            required
            className="w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100"
          />
        </label>
        <label className="flex-1 min-w-36">
          <span className="block text-sm text-surface-700 dark:text-surface-300 mb-1">Name</span>
          <input
            value={form.display_name}
            onChange={(e) => setForm({ ...form, display_name: e.target.value })}
            required
            className="w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100"
          />
        </label>
        <label className="flex-1 min-w-36">
          <span className="block text-sm text-surface-700 dark:text-surface-300 mb-1">Password</span>
          <input
            type="password"
            value={form.password}
            onChange={(e) => setForm({ ...form, password: e.target.value })}
            required
            minLength={8}
            className="w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100"
          />
        </label>
        <label>
          <span className="block text-sm text-surface-700 dark:text-surface-300 mb-1">Role</span>
          <select
            value={role}
            onChange={(e) => setRole(e.target.value)}
            className="px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100"
          >
            {TENANT_ROLES.map((r) => (
              <option key={r} value={r}>
                {r}
              </option>
            ))}
          </select>
        </label>
        <button
          type="submit"
          disabled={creating}
          className="flex items-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium disabled:opacity-50"
        >
          <Plus size={16} />
          {creating ? 'Adding…' : 'Add'}
        </button>
      </form>

      {error && (
        <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      <div className="mt-6 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 overflow-hidden">
        {loading ? (
          <p className="p-4 text-sm text-surface-600 dark:text-surface-400">Loading…</p>
        ) : users.length === 0 ? (
          <p className="p-4 text-sm text-surface-600 dark:text-surface-400">No users yet.</p>
        ) : (
          <ul className="divide-y divide-surface-200 dark:divide-surface-800">
            {users.map((user) => (
              <li key={user.id} className="flex items-center gap-4 p-4">
                <div className="flex-1 min-w-0">
                  <p className="text-sm font-medium text-surface-900 dark:text-surface-100 truncate">
                    {user.display_name}
                  </p>
                  <p className="text-xs text-surface-600 dark:text-surface-400 truncate">
                    {user.identities.map((i) => i.subject).join(', ') || 'no sign-in method'}
                  </p>
                </div>
                {user.system_roles.includes('system_admin') && (
                  <span
                    title="System administrator"
                    className="flex items-center gap-1 text-xs text-amber-600 dark:text-amber-400"
                  >
                    <Shield size={14} />
                    system
                  </span>
                )}
                <button
                  onClick={() => void onDelete(user.id)}
                  aria-label={`Delete ${user.display_name}`}
                  className="p-2 rounded-md text-surface-400 hover:text-red-600 dark:hover:text-red-400 hover:bg-surface-100 dark:hover:bg-surface-800"
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
