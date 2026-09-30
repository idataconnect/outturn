import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { Shield, UserRound } from 'lucide-react'

import { FilterBox, PageHeader, RecordList, RecordRow } from '../components/IndexPage'
import { ApiError, allPages } from '../lib/api'
import { matchesFilter } from '../lib/filter'
import { useSession } from '../lib/session'
import type { User } from './UserEditor'
import { paths } from '../lib/paths'

/**
 * The accounts this administrator may see, as a list.
 *
 * Creating, editing and deleting live on each user's own page, so this page
 * is only ever the list. The API scopes it: a workspace's
 * administrator sees the accounts holding a role in their workspace, a system
 * administrator sees everyone.
 */
export default function Users() {
  const state = useSession()
  const [users, setUsers] = useState<User[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [query, setQuery] = useState('')

  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canCreate = authorities.includes('users:create')
  const workspaceId = state.status === 'authenticated' ? state.session.workspace_id : null

  async function refresh() {
    try {
      setUsers(await allPages<User>('/v1/users'))
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

  const shown = users.filter((u) =>
    matchesFilter(query, u.display_name, ...u.identities.map((i) => i.subject)),
  )

  return (
    <div className="p-6 max-w-3xl">
      <PageHeader
        title="Users"
        description="The people who can sign in here, and what each may do."
        action={canCreate ? { to: paths.newUser, label: 'New user' } : undefined}
      />

      {error && (
        <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      {users.length > 0 && <FilterBox value={query} onChange={setQuery} label="Filter users" />}
      <RecordList
        loading={loading}
        count={users.length}
        shown={shown.length}
        empty={
          <>
            No users yet.
            {canCreate && (
              <>
                {' '}
                <Link to={paths.newUser} className="underline underline-offset-2">
                  Add one.
                </Link>
              </>
            )}
          </>
        }
      >
        {shown.map((user) => (
          <RecordRow
            key={user.id}
            to={paths.user(user.id)}
            icon={UserRound}
            title={user.display_name}
            aside={
              user.system_roles.includes('system_admin') && (
                <span
                  title="System administrator"
                  className="flex items-center gap-1 text-xs text-amber-600 dark:text-amber-400"
                >
                  <Shield size={14} aria-hidden />
                  system
                </span>
              )
            }
          >
            <p className="text-xs text-surface-600 dark:text-surface-400 truncate">
              {user.identities.map((i) => i.subject).join(', ') || 'no sign-in method'}
            </p>
          </RecordRow>
        ))}
      </RecordList>
    </div>
  )
}
