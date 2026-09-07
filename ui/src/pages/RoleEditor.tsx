import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { ArrowLeft, Save, Trash2 } from 'lucide-react'

import { ApiError, api } from '../lib/api'
import { useSession } from '../lib/session'
import type { TenantRole } from './Roles'

type AuthorityInfo = { name: string; description: string }

const field =
  'w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100'
const label = 'block text-sm text-surface-700 dark:text-surface-300 mb-1'

/** Groups `agents:create` under "agents", for a checklist a person can scan. */
function groupOf(name: string): string {
  return name.split(':')[0]
}

/**
 * Creating a role, or changing what one allows.
 *
 * The authorities on offer come from the API, so this page never has to know
 * the vocabulary. Ones the editor does not hold themselves are shown but
 * disabled: the server refuses them, and saying so up front beats a rejected
 * save.
 */
export default function RoleEditor() {
  const { id } = useParams<{ id?: string }>()
  const creating = id === undefined
  const navigate = useNavigate()
  const state = useSession()
  const mine = state.status === 'authenticated' ? state.session.authorities : []
  const canManage = mine.includes('roles:manage')

  const [vocabulary, setVocabulary] = useState<AuthorityInfo[]>([])
  const [role, setRole] = useState<TenantRole | null>(null)
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [chosen, setChosen] = useState<Set<string>>(() => new Set())
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let stale = false
    void (async () => {
      try {
        const [vocab, found] = await Promise.all([
          api<AuthorityInfo[]>('/v1/authorities'),
          creating ? Promise.resolve(null) : api<TenantRole>(`/v1/roles/${id}`),
        ])
        if (stale) return
        setVocabulary(vocab)
        if (found) {
          setRole(found)
          setName(found.name)
          setDescription(found.description)
          setChosen(new Set(found.authorities))
        }
      } catch (e) {
        if (!stale) setError(e instanceof ApiError ? e.message : 'failed to load role')
      } finally {
        if (!stale) setLoading(false)
      }
    })()
    return () => {
      stale = true
    }
  }, [id, creating])

  function toggle(authority: string) {
    setChosen((prev) => {
      const next = new Set(prev)
      if (next.has(authority)) next.delete(authority)
      else next.add(authority)
      return next
    })
  }

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault()
    setSaving(true)
    const body = { name, description, authorities: [...chosen].sort() }
    try {
      if (creating) {
        await api<TenantRole>('/v1/roles', { method: 'POST', body: JSON.stringify(body) })
      } else {
        await api<TenantRole>(`/v1/roles/${id}`, { method: 'PATCH', body: JSON.stringify(body) })
      }
      void navigate('/roles')
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to save role')
      setSaving(false)
    }
  }

  async function onDelete() {
    if (!role) return
    if (!window.confirm(`Delete the ${role.name} role?`)) return
    try {
      await api<void>(`/v1/roles/${id}`, { method: 'DELETE' })
      void navigate('/roles')
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to delete role')
    }
  }

  const groups = new Map<string, AuthorityInfo[]>()
  for (const a of vocabulary) {
    const g = groupOf(a.name)
    groups.set(g, [...(groups.get(g) ?? []), a])
  }

  return (
    <div className="p-6 max-w-3xl">
      <Link
        to="/roles"
        className="inline-flex items-center gap-1 text-sm text-surface-600 dark:text-surface-400 hover:text-surface-900 dark:hover:text-surface-100"
      >
        <ArrowLeft size={14} aria-hidden />
        Roles
      </Link>
      <h1 className="mt-2 text-2xl font-semibold text-surface-900 dark:text-surface-100">
        {creating ? 'New role' : (role?.name ?? 'Role')}
      </h1>

      {error && (
        <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      {loading ? (
        <p className="mt-6 text-sm text-surface-600 dark:text-surface-400">Loading…</p>
      ) : (
        <form
          onSubmit={onSubmit}
          className="mt-6 space-y-5 p-4 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900"
        >
          <div className="flex flex-wrap items-end gap-3">
            <label className="flex-1 min-w-44">
              <span className={label}>Name</span>
              <input
                value={name}
                onChange={(e) => setName(e.target.value)}
                required
                maxLength={64}
                disabled={!canManage}
                className={field}
              />
            </label>
            <label className="flex-[2] min-w-56">
              <span className={label}>Description</span>
              <input
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                disabled={!canManage}
                className={field}
              />
            </label>
          </div>

          <fieldset className="space-y-4">
            <legend className="text-sm font-semibold text-surface-900 dark:text-surface-100">
              What this role allows
            </legend>
            {[...groups.entries()].map(([group, items]) => (
              <div key={group}>
                <p className="mb-1 text-xs font-mono uppercase tracking-wide text-surface-500 dark:text-surface-400">
                  {group}
                </p>
                <div className="grid gap-1 sm:grid-cols-2">
                  {items.map((a) => {
                    const held = mine.includes(a.name)
                    return (
                      <label
                        key={a.name}
                        className={`flex items-start gap-2 text-sm ${
                          held
                            ? 'text-surface-700 dark:text-surface-300'
                            : 'text-surface-400 dark:text-surface-600'
                        }`}
                        // Not yours to give. The server refuses it too; this
                        // just says so before the click.
                        title={held ? a.description : `${a.description}. You do not hold this yourself, so you cannot put it in a role.`}
                      >
                        <input
                          type="checkbox"
                          className="mt-1"
                          checked={chosen.has(a.name)}
                          disabled={!canManage || !held}
                          onChange={() => toggle(a.name)}
                        />
                        <span>
                          {a.description}
                          <span className="block font-mono text-[11px] text-surface-500 dark:text-surface-500">
                            {a.name}
                          </span>
                        </span>
                      </label>
                    )
                  })}
                </div>
              </div>
            ))}
          </fieldset>

          {canManage && (
            <div className="flex items-center gap-3">
              <button
                type="submit"
                disabled={saving || name.trim() === ''}
                className="flex items-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium disabled:opacity-50"
              >
                <Save size={16} aria-hidden />
                {saving ? 'Saving…' : creating ? 'Create role' : 'Save changes'}
              </button>
              <Link
                to="/roles"
                className="text-sm text-surface-600 dark:text-surface-400 hover:text-surface-900 dark:hover:text-surface-100"
              >
                Cancel
              </Link>
              {role && (
                <button
                  type="button"
                  onClick={() => void onDelete()}
                  disabled={role.holders > 0}
                  title={
                    role.holders > 0
                      ? `${role.holders} ${role.holders === 1 ? 'person holds' : 'people hold'} this role; take it away from them first`
                      : undefined
                  }
                  className="ml-auto flex items-center gap-2 px-3 py-2 rounded-md text-sm text-red-700 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-950/40 disabled:opacity-50 disabled:hover:bg-transparent"
                >
                  <Trash2 size={16} aria-hidden />
                  Delete role
                </button>
              )}
            </div>
          )}
        </form>
      )}
    </div>
  )
}
