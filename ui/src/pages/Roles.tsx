import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { KeyRound, Plus } from 'lucide-react'

import { ApiError, api } from '../lib/api'
import { useSession } from '../lib/session'

export type WorkspaceRole = {
  id: string
  workspace_id: string
  name: string
  description: string
  authorities: string[]
  holders: number
}

/**
 * The roles this workspace has defined.
 *
 * Roles are the workspace's own: what they are called and what they allow is
 * decided here, not in code. Creating and editing live on their own routes;
 * deleting lives on the edit page, beside the name and the count of people
 * who hold it.
 */
export default function Roles() {
  const state = useSession()
  const [roles, setRoles] = useState<WorkspaceRole[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canManage = authorities.includes('roles:manage')
  const workspaceId = state.status === 'authenticated' ? state.session.workspace_id : null

  useEffect(() => {
    let stale = false
    void (async () => {
      try {
        const found = await api<WorkspaceRole[]>('/v1/roles')
        if (!stale) setRoles(found)
      } catch (e) {
        if (!stale) setError(e instanceof ApiError ? e.message : 'failed to load roles')
      } finally {
        if (!stale) setLoading(false)
      }
    })()
    return () => {
      stale = true
    }
  }, [workspaceId])

  return (
    <div className="p-6 max-w-3xl">
      <div className="flex items-start justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold text-surface-900 dark:text-surface-100">Roles</h1>
          <p className="mt-2 text-surface-600 dark:text-surface-400">
            What each role in this workspace allows. Changes apply to everyone holding the
            role on their next request.
          </p>
        </div>
        {canManage && (
          <Link
            to="/roles/new"
            className="flex items-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium"
          >
            <Plus size={16} aria-hidden />
            New role
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
        ) : roles.length === 0 ? (
          <p className="p-4 text-sm text-surface-600 dark:text-surface-400">No roles defined.</p>
        ) : (
          <ul className="divide-y divide-surface-200 dark:divide-surface-800">
            {roles.map((role) => (
              <li key={role.id} className="flex items-start gap-4 p-4">
                <KeyRound size={16} className="mt-1 shrink-0 text-surface-400" aria-hidden />
                <Link to={`/roles/${role.id}`} className="flex-1 min-w-0 group">
                  <p className="text-sm font-medium text-surface-900 dark:text-surface-100 truncate group-hover:underline underline-offset-2">
                    {role.name}
                    <span className="ml-2 text-xs font-normal text-surface-600 dark:text-surface-400">
                      {role.holders === 1 ? '1 person' : `${role.holders} people`},{' '}
                      {role.authorities.length === 1
                        ? '1 authority'
                        : `${role.authorities.length} authorities`}
                    </span>
                  </p>
                  {role.description && (
                    <p className="mt-1 text-xs text-surface-600 dark:text-surface-400 line-clamp-2">
                      {role.description}
                    </p>
                  )}
                </Link>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  )
}
