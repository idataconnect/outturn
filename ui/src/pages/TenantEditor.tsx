import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { ArrowLeft, Plus, Save, Trash2 } from 'lucide-react'

import { ApiError, api } from '../lib/api'

export type Tenant = {
  id: string
  name: string
  slug: string
}

const field =
  'w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100'
const label = 'block text-sm text-surface-700 dark:text-surface-300 mb-1'
const primary =
  'flex items-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium disabled:opacity-50'

function slugify(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
}

/**
 * Creating a tenant, or renaming and removing one.
 *
 * The slug is settled at creation: it names the tenant in URLs and in every
 * token minted for it, so it is shown afterwards but not editable.
 */
export default function TenantEditor() {
  const { id } = useParams<{ id?: string }>()
  const creating = id === undefined
  const navigate = useNavigate()

  const [name, setName] = useState('')
  const [slug, setSlug] = useState('')
  const [slugEdited, setSlugEdited] = useState(false)
  const [tenant, setTenant] = useState<Tenant | null>(null)
  const [loading, setLoading] = useState(!creating)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (creating) return
    let stale = false
    void (async () => {
      try {
        const found = await api<Tenant>(`/v1/tenants/${id}`)
        if (stale) return
        setTenant(found)
        setName(found.name)
        setSlug(found.slug)
      } catch (e) {
        if (!stale) setError(e instanceof ApiError ? e.message : 'failed to load tenant')
      } finally {
        if (!stale) setLoading(false)
      }
    })()
    return () => {
      stale = true
    }
  }, [id, creating])

  function onNameChange(value: string) {
    setName(value)
    if (creating && !slugEdited) setSlug(slugify(value))
  }

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault()
    setSaving(true)
    try {
      if (creating) {
        await api<Tenant>('/v1/tenants', {
          method: 'POST',
          body: JSON.stringify({ name, slug }),
        })
      } else {
        await api<Tenant>(`/v1/tenants/${id}`, {
          method: 'PATCH',
          body: JSON.stringify({ name }),
        })
      }
      void navigate('/tenants')
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to save tenant')
      setSaving(false)
    }
  }

  async function onDelete() {
    if (!tenant) return
    if (
      !window.confirm(
        `Delete ${tenant.name}? Every agent, session and role grant in it goes with it.`,
      )
    ) {
      return
    }
    try {
      await api<void>(`/v1/tenants/${id}`, { method: 'DELETE' })
      void navigate('/tenants')
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to delete tenant')
    }
  }

  const complete = name.trim() !== '' && slug.trim() !== ''

  return (
    <div className="p-6 max-w-3xl">
      <Link
        to="/tenants"
        className="inline-flex items-center gap-1 text-sm text-surface-600 dark:text-surface-400 hover:text-surface-900 dark:hover:text-surface-100"
      >
        <ArrowLeft size={14} aria-hidden />
        Tenants
      </Link>
      <h1 className="mt-2 text-2xl font-semibold text-surface-900 dark:text-surface-100">
        {creating ? 'New tenant' : (tenant?.name ?? 'Tenant')}
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
          className="mt-6 space-y-4 p-4 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900"
        >
          <div className="flex flex-wrap items-end gap-3">
            <label className="flex-1 min-w-48">
              <span className={label}>Name</span>
              <input
                value={name}
                onChange={(e) => onNameChange(e.target.value)}
                required
                className={field}
              />
            </label>
            <label className="flex-1 min-w-48">
              <span className={label}>Slug</span>
              <input
                value={slug}
                onChange={(e) => {
                  setSlugEdited(true)
                  setSlug(e.target.value)
                }}
                required
                pattern="[a-z0-9\-]+"
                disabled={!creating}
                className={`${field} font-mono text-sm disabled:opacity-60`}
              />
            </label>
          </div>
          <div className="flex items-center gap-3">
            <button type="submit" disabled={saving || !complete} className={primary}>
              {creating ? <Plus size={16} aria-hidden /> : <Save size={16} aria-hidden />}
              {saving ? 'Saving…' : creating ? 'Create tenant' : 'Save changes'}
            </button>
            <Link
              to="/tenants"
              className="text-sm text-surface-600 dark:text-surface-400 hover:text-surface-900 dark:hover:text-surface-100"
            >
              Cancel
            </Link>
            {!creating && (
              <button
                type="button"
                onClick={() => void onDelete()}
                className="ml-auto flex items-center gap-2 px-3 py-2 rounded-md text-sm text-red-700 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-950/40"
              >
                <Trash2 size={16} aria-hidden />
                Delete tenant
              </button>
            )}
          </div>
        </form>
      )}
    </div>
  )
}
