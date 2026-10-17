import { useCallback, useEffect, useRef, useState } from 'react'
import { BookOpen, RefreshCw, Upload } from 'lucide-react'
import { heldCatalog, importCatalog, refreshCatalog, type HeldCatalog } from '../lib/catalog'
import { configured, type Configured } from '../lib/models'
import Accounts from '../components/models/Accounts'
import Models from '../components/models/Models'

/** "3 days ago", "in 5 hours": near enough for an age, which is all this is. */
function relative(when: string, now = Date.now()): string {
  const seconds = (new Date(when).getTime() - now) / 1000
  const units: [Intl.RelativeTimeFormatUnit, number][] = [
    ['day', 86400],
    ['hour', 3600],
    ['minute', 60],
  ]
  const format = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' })
  for (const [unit, size] of units) {
    if (Math.abs(seconds) >= size) return format.format(Math.round(seconds / size), unit)
  }
  return format.format(Math.round(seconds), 'second')
}

/** Where the catalog came from, in words. */
function origin(source: string): string {
  if (source === 'shipped') return 'the copy this release shipped with'
  if (source === 'upload') return 'a file imported here'
  if (source.startsWith('file:')) return `the file at ${source.slice(5)}`
  return source
}

/**
 * The models this deployment answers with: the accounts requests go to, the
 * models configured at them, and the catalog their facts come from -- what
 * each accepts, how far it reaches. The catalog ships with every release, so
 * a deployment that can reach nothing still has one; this page says how
 * current it is.
 */
export default function PlatformModels() {
  const [held, setHeld] = useState<HeldCatalog | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [note, setNote] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const file = useRef<HTMLInputElement>(null)
  const [config, setConfig] = useState<Configured | null>(null)

  const loadConfig = useCallback(() => {
    configured()
      .then(setConfig)
      .catch((e: Error) => setError(e.message))
  }, [])
  useEffect(loadConfig, [loadConfig])

  const load = useCallback(() => {
    heldCatalog()
      .then((h) => {
        setHeld(h)
        setError(null)
      })
      .catch((e: Error) => setError(e.message))
  }, [])
  useEffect(load, [load])

  const run = async (what: () => Promise<{ checked: 'kept' | 'unchanged' }>) => {
    setBusy(true)
    setNote(null)
    setError(null)
    try {
      const { checked } = await what()
      setNote(checked === 'kept' ? 'A newer catalog was kept.' : 'Nothing has changed since the catalog held.')
      load()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="p-6 max-w-3xl space-y-6">
      <div>
        <h1 className="text-2xl font-semibold text-surface-900 dark:text-surface-100">Models</h1>
        <p className="mt-2 text-surface-600 dark:text-surface-400">
          Where this deployment's model requests go, and what it knows about the models providers offer.
          Visible to the operator only.
        </p>
      </div>

      {config && (
        <>
          <Accounts accounts={config.accounts} reload={loadConfig} />
          <Models accounts={config.accounts} models={config.models} reload={loadConfig} />
        </>
      )}

      <section className="rounded-lg border border-surface-200 dark:border-surface-700 p-4 space-y-4">
        <div className="flex items-start justify-between gap-4">
          <div className="flex items-center gap-2">
            <BookOpen size={18} className="text-surface-500" aria-hidden />
            <h2 className="text-lg font-medium text-surface-900 dark:text-surface-100">Catalog</h2>
          </div>
          <div className="flex shrink-0 gap-2">
            <button
              type="button"
              onClick={() => file.current?.click()}
              disabled={busy}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded-md border border-surface-300 dark:border-surface-600 text-sm text-surface-700 dark:text-surface-200 hover:bg-surface-50 dark:hover:bg-surface-800 disabled:opacity-50"
            >
              <Upload size={14} aria-hidden />
              Import file
            </button>
            <button
              type="button"
              onClick={() => void run(refreshCatalog)}
              disabled={busy || held?.fetching === false}
              title={held?.fetching === false ? 'Fetching is turned off for this deployment' : undefined}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium disabled:opacity-50"
            >
              <RefreshCw size={14} className={busy ? 'animate-spin' : undefined} aria-hidden />
              Check now
            </button>
            <input
              ref={file}
              type="file"
              accept=".json,.gz,application/json,application/gzip"
              className="hidden"
              onChange={(e) => {
                const chosen = e.target.files?.[0]
                e.target.value = ''
                if (chosen) void run(() => importCatalog(chosen))
              }}
            />
          </div>
        </div>

        {held && (
          <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-2 text-sm">
            <dt className="text-surface-500">Held</dt>
            <dd className="text-surface-900 dark:text-surface-100">
              {held.models.toLocaleString()} models from {held.providers.toLocaleString()} providers
            </dd>
            <dt className="text-surface-500">From</dt>
            <dd className="text-surface-900 dark:text-surface-100 [overflow-wrap:anywhere]">
              {origin(held.source)}
              {/* When a shipped copy was loaded says nothing about how old
                  it is -- only when this deployment first started. */}
              {held.source !== 'shipped' && `, ${relative(held.fetched_at)}`}
            </dd>
            <dt className="text-surface-500">Checked</dt>
            <dd className="text-surface-900 dark:text-surface-100">
              {!held.fetching
                ? 'Never: fetching is turned off for this deployment'
                : held.checked_at
                  ? relative(held.checked_at)
                  : 'Not yet'}
            </dd>
            {held.fetching && held.next_check_at && (
              <>
                <dt className="text-surface-500">Next check</dt>
                <dd className="text-surface-900 dark:text-surface-100">{relative(held.next_check_at)}</dd>
              </>
            )}
            {held.last_error && (
              <>
                <dt className="text-surface-500">Last check</dt>
                <dd className="text-red-700 dark:text-red-400 [overflow-wrap:anywhere]">{held.last_error}</dd>
              </>
            )}
          </dl>
        )}

        {note && (
          <p role="status" className="text-sm text-surface-600 dark:text-surface-400">
            {note}
          </p>
        )}
        {error && (
          <p role="alert" className="text-sm text-red-700 dark:text-red-400">
            {error}
          </p>
        )}
      </section>
    </div>
  )
}
