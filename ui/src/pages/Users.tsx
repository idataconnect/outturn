import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { Plus, Shield, Trash2 } from 'lucide-react'

import { ApiError, api } from '../lib/api'
import { useSession } from '../lib/session'
import type { User } from './UserEditor'

/**
 * The accounts this administrator may see, as a list.
 *
 * Creating and editing live on their own routes (`/users/new`, `/users/:id`),
 * so this page is only ever the list. The API scopes it: a workspace's
 * administrator sees the accounts holding a role in their workspace, a system
 * administrator sees everyone.
 */
export default function Users() {
  const state = useSession()
  const [users, setUsers] = useState<User[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canCreate = authorities.includes('users:create')
  const canDelete = authorities.includes('users:delete')
  const workspaceId = state.status === 'authenticated' ? state.session.workspace_id : null

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

  // The list is scoped to the workspace being viewed, so switching reloads it.
  useEffect(() => {
    void refresh()
  }, [workspaceId])

  async function onDelete(user: User) {
    if (!window.confirm(`Delete ${user.display_name}? Their sign-ins and roles go with them.`)) {
      return
    }
    try {
      await api<void>(`/v1/users/${user.id}`, { method: 'DELETE' })
      await refresh()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to delete user')
    }
  }

  return (
    <div className="p-6 max-w-3xl">
      <div className="flex items-start justify-between gap-4">
        <h1 className="text-2xl font-semibold text-surface-900 dark:text-surface-100">Users</h1>
        {canCreate && (
          <Link
            to="/users/new"
            className="flex items-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium"
          >
            <Plus size={16} aria-hidden />
            New user
          </Link>
        )}
      </div>

      {error && (
        <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      <div className="mt-6 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 overflow-hidden">
        {loading ? (
          <p className="p-4 text-sm text-surface-600 dark:text-surface-400">Loading…</p>
        ) : users.length === 0 ? (
          <p className="p-4 text-sm text-surface-600 dark:text-surface-400">
            No users yet.
            {canCreate && (
              <>
                {' '}
                <Link to="/users/new" className="underline underline-offset-2">
                  Add one.
                </Link>
              </>
            )}
          </p>
        ) : (
          <ul className="divide-y divide-surface-200 dark:divide-surface-800">
            {users.map((user) => (
              <li key={user.id} className="flex items-center gap-4 p-4">
                <Link to={`/users/${user.id}`} className="flex-1 min-w-0 group">
                  <p className="text-sm font-medium text-surface-900 dark:text-surface-100 truncate group-hover:underline underline-offset-2">
                    {user.display_name}
                  </p>
                  <p className="text-xs text-surface-600 dark:text-surface-400 truncate">
                    {user.identities.map((i) => i.subject).join(', ') || 'no sign-in method'}
                  </p>
                </Link>
                {user.system_roles.includes('system_admin') && (
                  <span
                    title="System administrator"
                    className="flex items-center gap-1 text-xs text-amber-600 dark:text-amber-400"
                  >
                    <Shield size={14} aria-hidden />
                    system
                  </span>
                )}
                {canDelete && (
                  <button
                    onClick={() => void onDelete(user)}
                    aria-label={`Delete ${user.display_name}`}
                    className="p-2 rounded-md text-surface-400 hover:text-red-600 dark:hover:text-red-400 hover:bg-surface-100 dark:hover:bg-surface-800"
                  >
                    <Trash2 size={16} aria-hidden />
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
