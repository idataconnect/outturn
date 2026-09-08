import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { Building2, Plus } from 'lucide-react'

import { ApiError, api } from '../lib/api'
import type { Workspace } from './WorkspaceEditor'

/**
 * Every workspace on the platform. System administrators only.
 *
 * Creating and editing live on their own routes (`/workspaces/new`,
 * `/workspaces/:id`), and deleting lives on the edit page beside the name it is
 * about to remove, rather than as a bin icon on a row.
 */
export default function Workspaces() {
  const [workspaces, setWorkspaces] = useState<Workspace[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    void (async () => {
      try {
        setWorkspaces(await api<Workspace[]>('/v1/workspaces'))
      } catch (e) {
        setError(e instanceof ApiError ? e.message : 'failed to load workspaces')
      } finally {
        setLoading(false)
      }
    })()
  }, [])

  return (
    <div className="p-6 max-w-3xl">
      <div className="flex items-start justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold text-surface-900 dark:text-surface-100">Workspaces</h1>
          <p className="mt-2 text-surface-600 dark:text-surface-400">
            Visible to system administrators only.
          </p>
        </div>
        <Link
          to="/workspaces/new"
          className="flex items-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium"
        >
          <Plus size={16} aria-hidden />
          New workspace
        </Link>
      </div>

      {error && (
        <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      <div className="mt-6 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 overflow-hidden">
        {loading ? (
          <p className="p-4 text-sm text-surface-600 dark:text-surface-400">Loading…</p>
        ) : workspaces.length === 0 ? (
          <p className="p-4 text-sm text-surface-600 dark:text-surface-400">
            No workspaces yet.{' '}
            <Link to="/workspaces/new" className="underline underline-offset-2">
              Create one.
            </Link>
          </p>
        ) : (
          <ul className="divide-y divide-surface-200 dark:divide-surface-800">
            {workspaces.map((workspace) => (
              <li key={workspace.id} className="flex items-center gap-4 p-4">
                <Building2 size={16} className="shrink-0 text-surface-400" aria-hidden />
                <Link to={`/workspaces/${workspace.id}`} className="flex-1 min-w-0 group">
                  <p className="text-sm font-medium text-surface-900 dark:text-surface-100 truncate group-hover:underline underline-offset-2">
                    {workspace.name}
                  </p>
                  <p className="text-xs font-mono text-surface-600 dark:text-surface-400 truncate">
                    {workspace.slug}
                  </p>
                </Link>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  )
}
