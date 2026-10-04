import { useCallback, useEffect, useState } from 'react'
import { Plug, Trash2 } from 'lucide-react'

import { Connection } from '../components/SkillConnections'
import { ApiError } from '../lib/api'
import {
  allowHost,
  listCredentials,
  listRules,
  removeRule,
  revokeCredential,
  type Credential,
  type EgressRule,
} from '../lib/credentials'
import { useSession } from '../lib/session'

/**
 * The hosts this workspace's agents may reach, and the key each is called
 * with, in one place.
 *
 * A skill's page shows the hosts that skill reaches; this shows all of them,
 * including ones no skill named and keys no rule is using any more. Allowing a
 * host is `settings:update`; connecting a key to it is `credentials:write` as
 * well, because sending a key is a second decision beside allowing the host.
 */
export default function Connections() {
  const state = useSession()
  const workspaceId = state.status === 'authenticated' ? state.session.workspace_id : null
  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canAllow = authorities.includes('settings:update')
  const canKey = authorities.includes('credentials:write')

  const [rules, setRules] = useState<EgressRule[] | null>(null)
  const [credentials, setCredentials] = useState<Credential[]>([])
  const [host, setHost] = useState('')
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(async () => {
    try {
      const [r, c] = await Promise.all([
        listRules(),
        listCredentials().catch((e) => {
          if (e instanceof ApiError && e.status === 403) return [] as Credential[]
          throw e
        }),
      ])
      setRules(r)
      setCredentials(c)
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to load connections')
    }
  }, [])

  useEffect(() => {
    void load()
  }, [load])

  async function act(work: () => Promise<unknown>) {
    try {
      await work()
      await load()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'that did not work')
    }
  }

  const used = new Set((rules ?? []).flatMap((r) => (r.credential ? [r.credential] : [])))
  const unused = credentials.filter((c) => !used.has(c.id) && c.revoked_at === null)

  return (
    <div className="p-6 max-w-3xl">
      <h1 className="flex items-center gap-2 text-2xl font-semibold text-surface-900 dark:text-surface-100">
        <Plug size={20} aria-hidden />
        Connections
      </h1>
      <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
        The hosts this workspace&apos;s agents may reach, and the key each is called with. A key is
        sealed in this browser so only the gateway can read it.
      </p>
      {error && (
        <p className="mt-3 text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      {canAllow && (
        <form
          className="mt-4 flex gap-2"
          onSubmit={(e) => {
            e.preventDefault()
            const h = host.trim()
            if (!h) return
            setHost('')
            void act(() => allowHost(h))
          }}
        >
          <input
            aria-label="Host to allow"
            value={host}
            onChange={(e) => setHost(e.target.value)}
            placeholder="api.example.com"
            className="flex-1 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 px-3 py-1.5 font-mono text-sm"
          />
          <button
            type="submit"
            disabled={!host.trim()}
            className="rounded-md bg-brand-700 px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50"
          >
            Allow host
          </button>
        </form>
      )}

      <div className="mt-4 space-y-3">
        {rules?.length === 0 && (
          <p className="text-sm text-surface-500 dark:text-surface-400">
            No hosts allowed. Agents in this workspace reach nothing outside it.
          </p>
        )}
        {(rules ?? []).map((rule) => (
          <ul key={rule.id} className="relative">
            {rule.credential_env ? (
              <li className="rounded-md border border-surface-200 dark:border-surface-800 px-3 py-3 text-sm">
                <span className="font-mono">{rule.host}</span>
                <span className="ml-2 text-xs text-surface-500">
                  key from the gateway&apos;s environment ({rule.credential_env})
                </span>
              </li>
            ) : canKey && workspaceId ? (
              <Connection
                rule={rule}
                credential={credentials.find((c) => c.id === rule.credential) ?? null}
                name={rule.host}
                workspaceId={workspaceId}
                onChanged={load}
              />
            ) : (
              <li className="rounded-md border border-surface-200 dark:border-surface-800 px-3 py-3 text-sm font-mono">
                {rule.host}
              </li>
            )}
            {canAllow && (
              <button
                type="button"
                aria-label={`Remove ${rule.host}`}
                title="Stop allowing this host"
                onClick={() => {
                  if (window.confirm(`Stop allowing ${rule.host}? Agents will be refused it.`)) {
                    void act(() => removeRule(rule.id))
                  }
                }}
                className="absolute right-2 bottom-2 rounded p-1 text-surface-400 hover:text-red-600"
              >
                <Trash2 size={14} aria-hidden />
              </button>
            )}
          </ul>
        ))}
      </div>

      {unused.length > 0 && (
        <section className="mt-8">
          <h2 className="text-sm font-medium text-surface-800 dark:text-surface-200">
            Keys no host uses
          </h2>
          <p className="mt-1 text-xs text-surface-600 dark:text-surface-400">
            Still stored, and still able to be put back on a host. Revoke one you no longer need; if
            it was ever exposed, revoke it with its provider too.
          </p>
          <ul className="mt-2 space-y-1">
            {unused.map((c) => (
              <li key={c.id} className="flex items-center gap-2 text-sm">
                <span>{c.name}</span>
                <span className="font-mono text-xs text-surface-500">
                  {(c.binding.hosts ?? []).join(', ')}
                </span>
                {canKey && (
                  <button
                    type="button"
                    onClick={() => void act(() => revokeCredential(c.id))}
                    className="ml-auto rounded-md border border-red-300 dark:border-red-900 px-2 py-0.5 text-xs text-red-700 dark:text-red-400"
                  >
                    Revoke
                  </button>
                )}
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  )
}
