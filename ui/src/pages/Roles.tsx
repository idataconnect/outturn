import { useEffect, useState } from 'react'
import { KeyRound } from 'lucide-react'

import { Badge, FilterBox, PageHeader, RecordList, RecordRow } from '../components/IndexPage'
import { ApiError, allPages } from '../lib/api'
import { matchesFilter } from '../lib/filter'
import { useSession } from '../lib/session'
import { paths } from '../lib/paths'

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
  const [query, setQuery] = useState('')

  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canManage = authorities.includes('roles:manage')
  const workspaceId = state.status === 'authenticated' ? state.session.workspace_id : null

  useEffect(() => {
    let stale = false
    void (async () => {
      try {
        const found = await allPages<WorkspaceRole>('/v1/roles')
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

  const shown = roles.filter((r) => matchesFilter(query, r.name, r.description))

  return (
    <div className="p-6 max-w-3xl">
      <PageHeader
        title="Roles"
        description="What each role in this workspace allows. Changes apply to everyone holding the role on their next request."
        action={canManage ? { to: paths.newRole, label: 'New role' } : undefined}
      />

      {error && (
        <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      {roles.length > 0 && <FilterBox value={query} onChange={setQuery} label="Filter roles" />}
      <RecordList loading={loading} count={roles.length} shown={shown.length} empty="No roles defined.">
        {shown.map((role) => (
          <RecordRow
            key={role.id}
            to={paths.role(role.id)}
            icon={KeyRound}
            title={role.name}
            badges={
              <Badge>
                {role.holders === 1 ? '1 person' : `${role.holders} people`},{' '}
                {role.authorities.length === 1
                  ? '1 authority'
                  : `${role.authorities.length} authorities`}
              </Badge>
            }
          >
            {role.description && (
              <p className="mt-1 text-xs text-surface-600 dark:text-surface-400 line-clamp-2">
                {role.description}
              </p>
            )}
          </RecordRow>
        ))}
      </RecordList>
    </div>
  )
}
