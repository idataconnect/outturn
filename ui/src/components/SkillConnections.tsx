import { useCallback, useEffect, useState } from 'react'
import { CheckCircle2, KeyRound, LoaderCircle, Plug, TriangleAlert } from 'lucide-react'

import { ApiError } from '../lib/api'
import {
  connect,
  disconnect,
  listCredentials,
  listRules,
  replaceKey,
  testCredential,
  type Credential,
  type EgressRule,
  type Tested,
} from '../lib/credentials'
import { httpFailure } from '../lib/httpFailure'

/**
 * The keys this skill's hosts are reached with, connected from here.
 *
 * A skill names the hosts it reaches and never a credential: the operator
 * publishes one skill, and each workspace connects its own key. So this is
 * where a workspace does that -- paste the key, and it is sealed in this
 * browser to the gateway, stored, and put on the host's rule, with no restart
 * anywhere. Then one GET says whether it works, so a wrong key is a red 401
 * here rather than an agent apologizing for it later.
 *
 * Only hosts the workspace already allows are shown; allowing one is the
 * banner above this.
 */
export default function SkillConnections({
  hosts,
  skillName,
  workspaceId,
}: {
  hosts: string[]
  skillName: string
  workspaceId: string
}) {
  const [rules, setRules] = useState<EgressRule[] | null>(null)
  const [credentials, setCredentials] = useState<Credential[]>([])
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(async () => {
    try {
      const [r, c] = await Promise.all([listRules(), listCredentials()])
      setRules(r)
      setCredentials(c)
      setError(null)
    } catch (e) {
      // A member without the authorities sees nothing here rather than an
      // error: connecting keys is an administrator's job.
      if (e instanceof ApiError && e.status === 403) setRules([])
      else setError(e instanceof ApiError ? e.message : 'failed to load connections')
    }
  }, [])

  useEffect(() => {
    void load()
  }, [load])

  const shown = (rules ?? []).filter((r) => hosts.includes(r.host))
  if (rules === null || shown.length === 0) return null

  return (
    <section className="mt-6" aria-labelledby="skill-connections">
      <h2
        id="skill-connections"
        className="flex items-center gap-2 text-sm font-medium text-surface-800 dark:text-surface-200"
      >
        <Plug size={14} aria-hidden />
        Connections
      </h2>
      <p className="mt-1 text-xs text-surface-600 dark:text-surface-400">
        The key each host is called with. It is sealed in this browser so only the gateway can read
        it, and nobody here can see it again once connected.
      </p>
      {error && (
        <p className="mt-2 text-xs text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}
      <ul className="mt-3 space-y-3">
        {shown.map((rule) => (
          <Connection
            key={rule.id}
            rule={rule}
            credential={credentials.find((c) => c.id === rule.credential) ?? null}
            name={`${skillName} -- ${rule.host}`}
            workspaceId={workspaceId}
            onChanged={load}
          />
        ))}
      </ul>
    </section>
  )
}

/** One host's key: connect it, test it, replace it, take it off. */
export function Connection({
  rule,
  credential,
  name,
  workspaceId,
  onChanged,
}: {
  rule: EgressRule
  credential: Credential | null
  /** What the credential is called if one is connected here. */
  name: string
  workspaceId: string
  onChanged: () => Promise<void>
}) {
  const connected = rule.credential !== null
  // Closed until asked for: plenty of hosts take no key at all, and a form
  // open on each of them reads as something left undone.
  const [editing, setEditing] = useState(false)
  const [header, setHeader] = useState(rule.header ?? 'authorization')
  const [secret, setSecret] = useState('')
  const [path, setPath] = useState('/')
  const [busy, setBusy] = useState(false)
  const [tested, setTested] = useState<Tested | null>(null)
  const [error, setError] = useState<string | null>(null)

  async function run(work: () => Promise<unknown>) {
    setBusy(true)
    setError(null)
    try {
      await work()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'that did not work')
    } finally {
      setBusy(false)
    }
  }

  const save = () =>
    run(async () => {
      const value = secret
      setSecret('')
      if (rule.credential) {
        await replaceKey(rule.credential, workspaceId, rule.host, header, value)
      } else {
        await connect(rule, workspaceId, name, header, value)
      }
      setEditing(false)
      setTested(null)
      await onChanged()
    })

  const test = () =>
    run(async () => {
      if (rule.credential) setTested(await testCredential(rule.credential, path))
    })

  const remove = () => {
    if (!window.confirm(`Disconnect the key for ${rule.host}? The host stays allowed.`)) return
    void run(async () => {
      await disconnect(rule)
      setEditing(false)
      setTested(null)
      await onChanged()
    })
  }

  return (
    <li className="rounded-md border border-surface-200 dark:border-surface-800 px-3 py-3 text-sm">
      <div className="flex items-center gap-2">
        <span className="font-mono text-surface-900 dark:text-surface-100">{rule.host}</span>
        {connected ? (
          <span className="ml-auto flex items-center gap-1 text-xs text-green-700 dark:text-green-400">
            <KeyRound size={12} aria-hidden />
            Connected, in <span className="font-mono">{rule.header}</span>
            {credential &&
              credential.generation > 1 &&
              ` (key replaced ${credential.generation - 1}×)`}
          </span>
        ) : (
          <span className="ml-auto text-xs text-surface-500 dark:text-surface-400">No key</span>
        )}
      </div>

      {editing ? (
        <form
          className="mt-3 space-y-2"
          onSubmit={(e) => {
            e.preventDefault()
            void save()
          }}
        >
          <label className="block text-xs text-surface-700 dark:text-surface-300">
            Header
            <input
              value={header}
              onChange={(e) => setHeader(e.target.value.trim().toLowerCase())}
              className="mt-1 w-full rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 px-2 py-1 font-mono text-xs"
            />
          </label>
          <label className="block text-xs text-surface-700 dark:text-surface-300">
            Value
            <input
              type="password"
              autoComplete="off"
              value={secret}
              onChange={(e) => setSecret(e.target.value)}
              placeholder={header === 'authorization' ? 'Bearer …' : ''}
              className="mt-1 w-full rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 px-2 py-1 font-mono text-xs"
            />
            <span className="mt-1 block text-surface-500 dark:text-surface-400">
              The whole header value, exactly as the API wants it. For most APIs that is{' '}
              <span className="font-mono">Bearer</span> followed by the key.
            </span>
          </label>
          <div className="flex gap-2">
            <button
              type="submit"
              disabled={busy || secret === '' || header === ''}
              className="rounded-md bg-brand-600 px-3 py-1.5 text-xs font-medium text-white disabled:opacity-50"
            >
              {connected ? 'Replace key' : 'Connect'}
            </button>
            {
              <button
                type="button"
                onClick={() => setEditing(false)}
                className="rounded-md border border-surface-300 dark:border-surface-700 px-3 py-1.5 text-xs"
              >
                Cancel
              </button>
            }
          </div>
        </form>
      ) : !connected ? (
        <button
          type="button"
          onClick={() => setEditing(true)}
          className="mt-2 rounded-md border border-surface-300 dark:border-surface-700 px-3 py-1.5 text-xs"
        >
          Connect a key
        </button>
      ) : (
        <div className="mt-3 space-y-2">
          <div className="flex items-center gap-2">
            <label className="flex flex-1 items-center gap-2 text-xs text-surface-700 dark:text-surface-300">
              Test with GET
              <input
                aria-label="Test path"
                value={path}
                onChange={(e) => setPath(e.target.value)}
                className="flex-1 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 px-2 py-1 font-mono text-xs"
              />
            </label>
            <button
              type="button"
              onClick={() => void test()}
              disabled={busy}
              className="rounded-md border border-surface-300 dark:border-surface-700 px-3 py-1.5 text-xs"
            >
              Test
            </button>
          </div>
          {tested && (
            <p
              className={`flex items-center gap-1.5 text-xs ${
                tested.ok ? 'text-green-700 dark:text-green-400' : 'text-red-700 dark:text-red-400'
              }`}
              role="status"
            >
              {tested.ok ? (
                <CheckCircle2 size={12} aria-hidden />
              ) : (
                <TriangleAlert size={12} aria-hidden />
              )}
              {tested.ok
                ? `The server answered ${tested.status}: the key works.`
                : (httpFailure(JSON.stringify({ status: tested.status })) ??
                  `The server answered ${tested.status}.`)}
              {tested.fingerprint && (
                <span
                  className="ml-1 font-mono text-surface-500 dark:text-surface-400"
                  title="The gateway's fingerprint of the key it sent. The same key always shows the same one; if this changes and nobody replaced the key, somebody else did."
                >
                  · key {tested.fingerprint}
                </span>
              )}
            </p>
          )}
          <div className="flex gap-2">
            <button
              type="button"
              onClick={() => setEditing(true)}
              className="rounded-md border border-surface-300 dark:border-surface-700 px-3 py-1.5 text-xs"
            >
              Replace key
            </button>
            <button
              type="button"
              onClick={remove}
              disabled={busy}
              className="rounded-md border border-red-300 dark:border-red-900 px-3 py-1.5 text-xs text-red-700 dark:text-red-400"
            >
              Disconnect
            </button>
          </div>
        </div>
      )}

      {busy && (
        <p className="mt-2 flex items-center gap-1 text-xs text-surface-500">
          <LoaderCircle size={12} className="animate-spin" aria-hidden /> Working…
        </p>
      )}
      {error && (
        <p className="mt-2 text-xs text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}
    </li>
  )
}
