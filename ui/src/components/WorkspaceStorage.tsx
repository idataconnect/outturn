import { useEffect, useId, useState } from 'react'

import { ApiError, api } from '../lib/api'
import { bytesLabel, type Storage } from '../lib/storage'

const field =
  'w-32 px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100'
const button =
  'px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 text-sm text-surface-700 dark:text-surface-300 hover:bg-surface-50 dark:hover:bg-surface-800 disabled:opacity-50'

/** Gigabytes, the unit a quota is chosen in. Powers of 1,000, as shown. */
const GB = 1_000_000_000

type Effective = { key: string; value: unknown; override_value?: unknown }

/**
 * One workspace's storage, for the operator: what it holds, and its quota --
 * the platform's default, or one set for this workspace. See
 * docs/storage-quota.md.
 */
export default function WorkspaceStorage({ workspaceId }: { workspaceId: string }) {
  const [storage, setStorage] = useState<Storage | null>(null)
  const [own, setOwn] = useState<number | null>(null)
  const [gb, setGb] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const ids = { quota: useId() }

  // Moved on by a save, so what was written is read back.
  const [reloads, setReloads] = useState(0)

  useEffect(() => {
    let stale = false
    void Promise.all([
      api<Storage>(`/v1/workspaces/${workspaceId}/storage`),
      api<Effective[]>(`/v1/workspaces/${workspaceId}/settings`),
    ])
      .then(([s, settings]) => {
        if (stale) return
        setStorage(s)
        const quota = settings.find((e) => e.key === 'max_stored_bytes')
        const set = typeof quota?.override_value === 'number' ? quota.override_value : null
        setOwn(set)
        setGb(set === null ? '' : String(set / GB))
      })
      .catch((e) => !stale && setError(e instanceof ApiError ? e.message : 'storage could not be read'))
    return () => {
      stale = true
    }
  }, [workspaceId, reloads])

  async function save(clear: boolean) {
    setError(null)
    const n = Number(gb)
    if (!clear && !(gb.trim() !== '' && Number.isFinite(n) && n >= 0)) {
      setError('The quota is a number of gigabytes, or 0 for no limit.')
      return
    }
    setBusy(true)
    try {
      await api(`/v1/workspaces/${workspaceId}/settings/max_stored_bytes`, {
        method: clear ? 'DELETE' : 'PUT',
        body: clear ? undefined : JSON.stringify({ value: Math.round(n * GB) }),
      })
      setReloads((n) => n + 1)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'the quota could not be saved')
    } finally {
      setBusy(false)
    }
  }

  return (
    <section className="mt-6 p-4 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900">
      <h2 className="text-base font-semibold text-surface-900 dark:text-surface-100">Storage</h2>
      {storage && (
        <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
          Holds {bytesLabel(storage.stored_bytes)}
          {storage.quota > 0 ? ` of ${bytesLabel(storage.quota)}` : ', with no limit'}
          {storage.full ? '. Full: files can be read and deleted, but not added.' : '.'}
          {!storage.counted && ' Not counted from storage yet, so no quota applies until it is.'}
        </p>
      )}
      <div className="mt-3 flex flex-wrap items-end gap-2">
        <label htmlFor={ids.quota} className="text-sm text-surface-700 dark:text-surface-300">
          <span className="mb-1 block">Quota for this workspace, in GB</span>
          <input
            id={ids.quota}
            inputMode="decimal"
            placeholder={own === null ? 'Platform default' : undefined}
            value={gb}
            onChange={(e) => setGb(e.target.value)}
            className={field}
          />
        </label>
        <button type="button" disabled={busy} className={button} onClick={() => void save(false)}>
          Save quota
        </button>
        {own !== null && (
          <button type="button" disabled={busy} className={button} onClick={() => void save(true)}>
            Use the platform default
          </button>
        )}
      </div>
      {error && (
        <p role="alert" className="mt-2 text-sm text-red-600 dark:text-red-400">
          {error}
        </p>
      )}
    </section>
  )
}
