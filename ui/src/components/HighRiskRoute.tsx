import { useEffect, useState } from 'react'

import { ApiError } from '../lib/api'
import { getRoutes, putHighRoute, type Routes } from '../lib/autoApproval'

/**
 * Who is asked about a high-risk request: roles the workspace names, each of
 * which must answer approvals. None named asks every approver, as a medium
 * request does, so leaving this alone changes nothing.
 */
export default function HighRiskRoute() {
  const [routes, setRoutes] = useState<Routes | null>(null)
  const [chosen, setChosen] = useState<ReadonlySet<string>>(new Set())
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [saved, setSaved] = useState(false)

  useEffect(() => {
    let stale = false
    getRoutes()
      .then((r) => {
        if (stale) return
        setRoutes(r)
        setChosen(new Set(r.high.map((h) => h.role_id)))
      })
      .catch((e) => {
        if (!stale) setError(e instanceof ApiError ? e.message : 'failed to load who is asked')
      })
    return () => {
      stale = true
    }
  }, [])

  async function save() {
    setBusy(true)
    setError(null)
    setSaved(false)
    try {
      const r = await putHighRoute([...chosen])
      setRoutes(r)
      setChosen(new Set(r.high.map((h) => h.role_id)))
      setSaved(true)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'that could not be saved')
    } finally {
      setBusy(false)
    }
  }

  const unchanged =
    routes !== null &&
    chosen.size === routes.high.length &&
    routes.high.every((h) => chosen.has(h.role_id))

  return (
    <section aria-labelledby="high-risk-heading" className="mt-10">
      <h2 id="high-risk-heading" className="text-lg font-semibold text-surface-900 dark:text-surface-100">
        Who is asked about high-risk requests
      </h2>
      <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
        A high-risk request asks the roles chosen here. With none chosen, or none of them left able to answer,
        it asks everybody who answers approvals, as a medium-risk request does.
      </p>
      {error && (
        <p role="alert" className="mt-2 text-sm text-red-600 dark:text-red-400">
          {error}
        </p>
      )}
      {routes && routes.answerers.length === 0 && (
        <p className="mt-3 text-xs text-surface-600 dark:text-surface-400">No role here answers approvals.</p>
      )}
      {routes && routes.answerers.length > 0 && (
        <fieldset disabled={!routes.may_write || busy} className="mt-3 space-y-1.5">
          <legend className="sr-only">Roles asked about high-risk requests</legend>
          {routes.answerers.map((role) => (
            <label key={role.role_id} className="flex items-center gap-2 text-sm text-surface-800 dark:text-surface-200">
              <input
                type="checkbox"
                checked={chosen.has(role.role_id)}
                onChange={(e) => {
                  const next = new Set(chosen)
                  if (e.target.checked) next.add(role.role_id)
                  else next.delete(role.role_id)
                  setChosen(next)
                  setSaved(false)
                }}
              />
              {role.name}
            </label>
          ))}
        </fieldset>
      )}
      {routes?.may_write && routes.answerers.length > 0 && (
        <div className="mt-3 flex items-center gap-3">
          <button
            type="button"
            disabled={busy || unchanged}
            onClick={() => void save()}
            className="rounded-md bg-brand-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-brand-700 disabled:opacity-50"
          >
            {busy ? 'Saving…' : 'Save'}
          </button>
          {saved && (
            <span role="status" className="text-xs text-surface-600 dark:text-surface-400">
              Saved. From the next request raised.
            </span>
          )}
        </div>
      )}
      {routes && !routes.may_write && (
        <p className="mt-2 text-xs text-surface-600 dark:text-surface-400">
          Changed only by people who may set policies.
        </p>
      )}
    </section>
  )
}
