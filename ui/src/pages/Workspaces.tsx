import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { Building2 } from 'lucide-react'

import { FilterBox, PageHeader, RecordList, RecordRow } from '../components/IndexPage'
import { ApiError, allPages } from '../lib/api'
import { matchesFilter } from '../lib/filter'
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
  const [query, setQuery] = useState('')

  useEffect(() => {
    void (async () => {
      try {
        setWorkspaces(await allPages<Workspace>('/v1/workspaces'))
      } catch (e) {
        setError(e instanceof ApiError ? e.message : 'failed to load workspaces')
      } finally {
        setLoading(false)
      }
    })()
  }, [])

  const shown = workspaces.filter((w) => matchesFilter(query, w.name, w.slug))

  return (
    <div className="p-6 max-w-3xl">
      <PageHeader
        title="Workspaces"
        description="Every workspace on the platform. Visible to system administrators only."
        action={{ to: '/workspaces/new', label: 'New workspace' }}
      />

      {error && (
        <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      {workspaces.length > 0 && (
        <FilterBox value={query} onChange={setQuery} label="Filter workspaces" />
      )}
      <RecordList
        loading={loading}
        count={workspaces.length}
        shown={shown.length}
        empty={
          <>
            No workspaces yet.{' '}
            <Link to="/workspaces/new" className="underline underline-offset-2">
              Create one.
            </Link>
          </>
        }
      >
        {shown.map((workspace) => (
          <RecordRow
            key={workspace.id}
            to={`/workspaces/${workspace.id}`}
            icon={Building2}
            title={workspace.name}
          >
            <p className="text-xs font-mono text-surface-600 dark:text-surface-400 truncate">
              {workspace.slug}
            </p>
          </RecordRow>
        ))}
      </RecordList>
    </div>
  )
}
