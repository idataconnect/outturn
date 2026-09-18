import { useCallback, useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { ArrowLeft, Plus, Save, Shield, Trash2 } from 'lucide-react'

import { ApiError, api } from '../lib/api'
import { useSession } from '../lib/session'

export type Identity = {
  id: string
  provider: string
  subject: string
  verified: boolean
}

export type User = {
  id: string
  display_name: string
  identities: Identity[]
  system_roles: string[]
}

type Membership = {
  workspace_id: string
  name: string
  slug: string
  roles: string[]
}

type UserDetail = User & { memberships: Membership[] }

type WorkspaceRoleOption = { id: string; name: string; description: string }

/**
 * The workspace's roles, for a picker. Empty when the caller may not see them
 * -- listing roles takes roles:assign or roles:manage -- in which case the
 * picker is not shown at all.
 */
function useWorkspaceRoles(): WorkspaceRoleOption[] {
  const state = useSession()
  const workspaceId = state.status === 'authenticated' ? state.session.workspace_id : null
  const allowed =
    state.status === 'authenticated' &&
    (state.session.authorities.includes('roles:assign') ||
      state.session.authorities.includes('roles:manage'))
  const [roles, setRoles] = useState<WorkspaceRoleOption[]>([])
  useEffect(() => {
    if (!allowed) return
    let stale = false
    void api<WorkspaceRoleOption[]>('/v1/roles')
      .then((found) => {
        if (!stale) setRoles(found)
      })
      .catch(() => {})
    return () => {
      stale = true
    }
  }, [workspaceId, allowed])
  return roles
}

const field =
  'w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100'
const label = 'block text-sm text-surface-700 dark:text-surface-300 mb-1'
const primary =
  'flex items-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium disabled:opacity-50'
const card =
  'mt-6 space-y-4 p-4 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900'

function message(e: unknown, fallback: string): string {
  return e instanceof ApiError ? e.message : fallback
}

/**
 * Creating a user, or looking after one.
 *
 * `/users/new` makes an account with its first sign-in and a role in the
 * current workspace. `/users/:id` shows the account: its name, every way it can
 * sign in, and its roles in the workspace being viewed. Each section saves on
 * its own -- an identity is added or removed the moment you say so, a role
 * likewise -- because they are separate facts about the account and a single
 * "save" button would have to pretend otherwise.
 */
export default function UserEditor() {
  const { id } = useParams<{ id?: string }>()
  const creating = id === undefined
  return creating ? <CreateUser /> : <EditUser id={id} />
}

function CreateUser() {
  const navigate = useNavigate()
  const state = useSession()
  const workspaceId = state.status === 'authenticated' ? state.session.workspace_id : null
  const manyWorkspaces =
    state.status === 'authenticated' && state.session.workspaces.length > 1

  const roles = useWorkspaceRoles()
  const [form, setForm] = useState({ email: '', display_name: '', password: '' })
  const [role, setRole] = useState<string>('')
  const [saving, setSaving] = useState(false)
  // Default to the least the workspace offers, once the list is known.
  useEffect(() => {
    if (role === '' && roles.length > 0) {
      const least = [...roles].sort((a, b) => a.name.localeCompare(b.name)).at(-1)
      if (least) setRole(least.name)
    }
  }, [roles, role])
  const [error, setError] = useState<string | null>(null)

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault()
    if (!workspaceId) return
    setSaving(true)
    try {
      // The role is granted in the same request, in the workspace the admin is
      // currently scoped to. Two requests left a window where the account
      // existed with no role here and so vanished from the creator's list.
      const user = await api<User>('/v1/users', {
        method: 'POST',
        body: JSON.stringify({ ...form, role }),
      })
      void navigate(`/users/${user.id}`)
    } catch (e) {
      setError(message(e, 'failed to create user'))
      setSaving(false)
    }
  }

  return (
    <div className="p-6 max-w-3xl">
      <Back />
      <h1 className="mt-2 text-2xl font-semibold text-surface-900 dark:text-surface-100">New user</h1>
      <p className="mt-2 text-surface-600 dark:text-surface-400">
        {manyWorkspaces
          ? 'The role is granted in the workspace you are currently viewing.'
          : 'The role is granted here.'}
      </p>

      {error && <Error text={error} />}

      <form onSubmit={onSubmit} className={card}>
        <div className="flex flex-wrap items-end gap-3">
          <label className="flex-1 min-w-44">
            <span className={label}>Email</span>
            <input
              type="email"
              value={form.email}
              onChange={(e) => setForm({ ...form, email: e.target.value })}
              required
              className={field}
            />
          </label>
          <label className="flex-1 min-w-36">
            <span className={label}>Name</span>
            <input
              value={form.display_name}
              onChange={(e) => setForm({ ...form, display_name: e.target.value })}
              required
              className={field}
            />
          </label>
        </div>
        <div className="flex flex-wrap items-end gap-3">
          <label className="flex-1 min-w-36">
            <span className={label}>Password</span>
            <input
              type="password"
              value={form.password}
              onChange={(e) => setForm({ ...form, password: e.target.value })}
              required
              minLength={8}
              className={field}
            />
          </label>
          <label>
            <span className={label}>Role</span>
            <RoleSelect roles={roles} value={role} onChange={setRole} />
          </label>
        </div>
        <div className="flex items-center gap-3">
          <button type="submit" disabled={saving || role === ''} className={primary}>
            <Plus size={16} aria-hidden />
            {saving ? 'Adding…' : 'Add user'}
          </button>
          <Cancel />
        </div>
      </form>
    </div>
  )
}

function EditUser({ id }: { id: string }) {
  const state = useSession()
  const workspaceId = state.status === 'authenticated' ? state.session.workspace_id : null
  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  // The session's id is the account id: that is what the login path mints
  // the token's subject from.
  const self = state.status === 'authenticated' && state.session.session_id === id
  // The API lets an account manage its own identities and name without the
  // update authority, so the form follows the same rule.
  const canUpdate = self || authorities.includes('users:update')
  const canAssign = authorities.includes('roles:assign')

  const roles = useWorkspaceRoles()
  const [user, setUser] = useState<UserDetail | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [name, setName] = useState('')
  const [savingName, setSavingName] = useState(false)
  const [identity, setIdentity] = useState({ email: '', password: '' })
  const [addingIdentity, setAddingIdentity] = useState(false)

  const load = useCallback(async () => {
    try {
      const detail = await api<UserDetail>(`/v1/users/${id}`)
      setUser(detail)
      setName(detail.display_name)
      setError(null)
    } catch (e) {
      setError(message(e, 'failed to load user'))
    }
  }, [id])

  useEffect(() => {
    void load()
  }, [load])

  // Separate from `load`, because it needs an authority the page does not
  // require: somebody who may read a user but not assign their roles sees the
  // rest of the page without this.
  useEffect(() => {
    void loadScope()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, canAssign, workspaceId])

  async function onRename(event: React.FormEvent) {
    event.preventDefault()
    setSavingName(true)
    try {
      await api<User>(`/v1/users/${id}`, {
        method: 'PATCH',
        body: JSON.stringify({ display_name: name }),
      })
      await load()
    } catch (e) {
      setError(message(e, 'failed to rename user'))
    } finally {
      setSavingName(false)
    }
  }

  async function onAddIdentity(event: React.FormEvent) {
    event.preventDefault()
    setAddingIdentity(true)
    try {
      await api<Identity>(`/v1/users/${id}/identities`, {
        method: 'POST',
        body: JSON.stringify(identity),
      })
      setIdentity({ email: '', password: '' })
      await load()
    } catch (e) {
      setError(message(e, 'failed to add sign-in'))
    } finally {
      setAddingIdentity(false)
    }
  }

  async function onRemoveIdentity(identity: Identity) {
    if (!window.confirm(`Remove ${identity.subject} as a way to sign in?`)) return
    try {
      await api<void>(`/v1/users/${id}/identities/${identity.id}`, { method: 'DELETE' })
      await load()
    } catch (e) {
      setError(message(e, 'failed to remove sign-in'))
    }
  }

  /** Which agents this person is confined to, empty meaning nobody confined them. */
  const [scoped, setScoped] = useState<string[]>([])
  const [agents, setAgents] = useState<{ id: string; name: string }[]>([])

  async function loadScope() {
    if (!canAssign) return
    try {
      const [rows, list] = await Promise.all([
        api<{ user_id: string; agents: string[] }[]>('/v1/scopes'),
        api<{ id: string; name: string }[]>('/v1/agents'),
      ])
      setAgents(list)
      setScoped(rows.find((r) => r.user_id === id)?.agents ?? [])
    } catch (e) {
      setError(message(e, 'failed to load agent access'))
    }
  }

  async function onToggleAgent(agentId: string, held: boolean) {
    // The whole set is sent, because the endpoint replaces rather than merges:
    // saying what somebody may reach is one decision, not a running total.
    const next = held ? scoped.filter((a) => a !== agentId) : [...scoped, agentId]
    try {
      await api<void>(`/v1/scopes/${id}`, {
        method: 'PUT',
        body: JSON.stringify({ agents: next }),
      })
      setScoped(next)
    } catch (e) {
      setError(message(e, 'failed to change agent access'))
    }
  }

  async function onToggleRole(role: string, held: boolean) {
    if (!workspaceId) return
    try {
      if (held) {
        await api<void>(`/v1/users/${id}/workspaces/${workspaceId}/roles/${role}`, { method: 'DELETE' })
      } else {
        await api<void>(`/v1/users/${id}/workspaces/${workspaceId}/roles`, {
          method: 'POST',
          body: JSON.stringify({ role }),
        })
      }
      await load()
    } catch (e) {
      setError(message(e, 'failed to change role'))
    }
  }

  const here = user?.memberships.find((m) => m.workspace_id === workspaceId)
  const heldRoles = new Set(here?.roles ?? [])
  const isSystem = user?.system_roles.includes('system_admin') ?? false
  // Workspaces with roles actually held, which is not every row: a system
  // administrator is listed against all of them, holding roles in only some.
  const elsewhere = (user?.memberships ?? []).filter(
    (m) => m.workspace_id !== workspaceId && m.roles.length > 0,
  )

  return (
    <div className="p-6 max-w-3xl">
      <Back />
      <h1 className="mt-2 flex items-center gap-3 text-2xl font-semibold text-surface-900 dark:text-surface-100">
        {user?.display_name ?? 'User'}
        {isSystem && (
          <span
            title="System administrator"
            className="flex items-center gap-1 text-xs font-normal text-amber-600 dark:text-amber-400"
          >
            <Shield size={14} aria-hidden />
            system
          </span>
        )}
      </h1>

      {error && <Error text={error} />}

      {user && (
        <>
          <form onSubmit={onRename} className={card}>
            <label className="block">
              <span className={label}>Name</span>
              <input
                value={name}
                onChange={(e) => setName(e.target.value)}
                required
                disabled={!canUpdate}
                className={field}
              />
            </label>
            {canUpdate && (
              <button
                type="submit"
                disabled={savingName || name.trim() === '' || name === user.display_name}
                className={primary}
              >
                <Save size={16} aria-hidden />
                {savingName ? 'Saving…' : 'Save name'}
              </button>
            )}
          </form>

          <section className={card}>
            <h2 className="text-sm font-semibold text-surface-900 dark:text-surface-100">
              Ways to sign in
            </h2>
            {user.identities.length === 0 ? (
              <p className="text-sm text-surface-600 dark:text-surface-400">
                None. This account cannot sign in.
              </p>
            ) : (
              <ul className="divide-y divide-surface-200 dark:divide-surface-800 -mx-4">
                {user.identities.map((i) => (
                  <li key={i.id} className="flex items-center gap-4 px-4 py-2">
                    <div className="flex-1 min-w-0">
                      <p className="text-sm text-surface-900 dark:text-surface-100 truncate">
                        {i.subject}
                      </p>
                      <p className="text-xs text-surface-600 dark:text-surface-400">
                        {i.provider}
                        {i.verified ? ', verified' : ''}
                      </p>
                    </div>
                    {canUpdate && (
                      <button
                        type="button"
                        onClick={() => void onRemoveIdentity(i)}
                        aria-label={`Remove ${i.subject}`}
                        // The API keeps the last one; the button stays so the
                        // refusal is explained rather than silently absent.
                        className="p-2 rounded-md text-surface-400 hover:text-red-600 dark:hover:text-red-400 hover:bg-surface-100 dark:hover:bg-surface-800"
                      >
                        <Trash2 size={16} aria-hidden />
                      </button>
                    )}
                  </li>
                ))}
              </ul>
            )}
            {canUpdate && (
              <form onSubmit={onAddIdentity} className="flex flex-wrap items-end gap-3">
                <label className="flex-1 min-w-44">
                  <span className={label}>Email</span>
                  <input
                    type="email"
                    value={identity.email}
                    onChange={(e) => setIdentity({ ...identity, email: e.target.value })}
                    required
                    className={field}
                  />
                </label>
                <label className="flex-1 min-w-36">
                  <span className={label}>Password</span>
                  <input
                    type="password"
                    value={identity.password}
                    onChange={(e) => setIdentity({ ...identity, password: e.target.value })}
                    required
                    minLength={8}
                    className={field}
                  />
                </label>
                <button type="submit" disabled={addingIdentity} className={primary}>
                  <Plus size={16} aria-hidden />
                  {addingIdentity ? 'Adding…' : 'Add sign-in'}
                </button>
              </form>
            )}
          </section>

          <section className={card}>
            <h2 className="text-sm font-semibold text-surface-900 dark:text-surface-100">
              Roles {here ? `in ${here.name}` : 'here'}
            </h2>
            {/* Roles are toggled directly. A grant is a fact about the
                account from the moment it is made, and a form that batched
                them would show a state the server did not yet hold. */}
            {roles.length === 0 ? (
              <p className="text-sm text-surface-600 dark:text-surface-400">
                {[...heldRoles].join(', ') || 'None'}
              </p>
            ) : (
              <div className="grid gap-2 sm:grid-cols-2">
                {roles.map((role) => (
                  <label
                    key={role.id}
                    className="flex items-start gap-2 text-sm text-surface-700 dark:text-surface-300"
                    title={role.description}
                  >
                    <input
                      type="checkbox"
                      className="mt-1"
                      checked={heldRoles.has(role.name)}
                      disabled={!canAssign}
                      onChange={() => void onToggleRole(role.name, heldRoles.has(role.name))}
                    />
                    <span>
                      {role.name}
                      {role.description && (
                        <span className="block text-xs text-surface-500 dark:text-surface-400">
                          {role.description}
                        </span>
                      )}
                    </span>
                  </label>
                ))}
              </div>
            )}
            {/* A system administrator's memberships list every workspace,
                granted or not: the right to sign in anywhere comes from
                `user_system_roles` rather than from a grant in each one. Those
                rows carry no roles, and calling them membership would name the
                wrong thing -- so the reach is said once, and only the
                workspaces where roles are actually held are listed. */}
            {/* Which agents this person's authorities reach. Absent for
                everybody by default: an unnarrowed person holds what their
                roles say across the workspace, and showing that as "every
                agent ticked" would make the ordinary case look like a decision
                somebody took. */}
            {canAssign && agents.length > 0 && (
              <div className="mt-6">
                <h3 className="text-sm font-medium text-surface-900 dark:text-surface-100">
                  Agent access
                </h3>
                <p className="mt-1 text-xs text-surface-600 dark:text-surface-400">
                  {scoped.length === 0
                    ? 'Every agent in this workspace. Tick some to confine them to those.'
                    : 'Confined to the agents ticked. Untick them all to restore the rest.'}
                </p>
                <div className="mt-3 grid gap-2 sm:grid-cols-2">
                  {agents.map((agent) => (
                    <label
                      key={agent.id}
                      className="flex items-start gap-2 text-sm text-surface-700 dark:text-surface-300"
                    >
                      <input
                        type="checkbox"
                        className="mt-1"
                        checked={scoped.includes(agent.id)}
                        onChange={() => void onToggleAgent(agent.id, scoped.includes(agent.id))}
                      />
                      <span>{agent.name}</span>
                    </label>
                  ))}
                </div>
                <p className="mt-2 text-xs text-surface-500 dark:text-surface-400">
                  Everyone sees which agents exist. This decides whose
                  conversations and files they may read, and which agents they
                  may talk to.
                </p>
              </div>
            )}
            {isSystem && (
              <p className="text-xs text-surface-500 dark:text-surface-400">
                A system administrator, so may sign in to any workspace.
              </p>
            )}
            {elsewhere.length > 0 && (
              <p className="text-xs text-surface-500 dark:text-surface-400">
                Also a member of{' '}
                {elsewhere.map((m) => `${m.name} (${m.roles.join(', ')})`).join('; ')}.
              </p>
            )}
          </section>
        </>
      )}
    </div>
  )
}

function RoleSelect({
  roles,
  value,
  onChange,
}: {
  roles: WorkspaceRoleOption[]
  value: string
  onChange: (v: string) => void
}) {
  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      disabled={roles.length === 0}
      className="px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100 disabled:opacity-50"
    >
      {roles.length === 0 && <option value="">Loading roles…</option>}
      {roles.map((r) => (
        <option key={r.id} value={r.name} title={r.description}>
          {r.name}
        </option>
      ))}
    </select>
  )
}

function Back() {
  return (
    <Link
      to="/users"
      className="inline-flex items-center gap-1 text-sm text-surface-600 dark:text-surface-400 hover:text-surface-900 dark:hover:text-surface-100"
    >
      <ArrowLeft size={14} aria-hidden />
      Users
    </Link>
  )
}

function Cancel() {
  return (
    <Link
      to="/users"
      className="text-sm text-surface-600 dark:text-surface-400 hover:text-surface-900 dark:hover:text-surface-100"
    >
      Cancel
    </Link>
  )
}

function Error({ text }: { text: string }) {
  return (
    <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
      {text}
    </p>
  )
}
