import { useCallback, useEffect, useState } from 'react'
import { KeyRound, LoaderCircle, Plug } from 'lucide-react'

import { ApiError } from '../lib/api'
import {
  connectOwn,
  disconnectOwn,
  listOwnConnections,
  type OwnConnection,
  type OwnConnections as Listed,
} from '../lib/credentials'

/**
 * The keys this person connected for services that check who is asking.
 *
 * An administrator decides a host's connection is each person's own; each
 * person then connects their key here, sealed in this browser as theirs, so
 * the service applies its own permissions to what they ask for. Their key goes
 * only with turns they start, never with anybody else's, and leaving the
 * workspace revokes it. See docs/user-credentials.md.
 *
 * Shown only when the workspace has such a host: for most people there is
 * nothing to connect, and an empty section reads as something left undone.
 */
export default function OwnConnections({ workspaceId, userId }: { workspaceId: string; userId: string }) {
  const [listed, setListed] = useState<Listed | null>(null)
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(async () => {
    try {
      setListed(await listOwnConnections())
      setError(null)
    } catch (e) {
      // Not a member here -- a system administrator visiting -- has nothing
      // of their own to connect.
      if (e instanceof ApiError && e.status === 403) setListed({ connections: [], seal_key: null })
      else setError(e instanceof ApiError ? e.message : 'failed to load connections')
    }
  }, [])

  useEffect(() => {
    void load()
  }, [load])

  if (error) {
    return (
      <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
        {error}
      </p>
    )
  }
  if (!listed || listed.connections.length === 0) return null

  return (
    <section
      className="mt-6 space-y-3 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 p-4"
      aria-labelledby="own-connections"
    >
      <h2
        id="own-connections"
        className="flex items-center gap-2 text-sm font-semibold text-surface-900 dark:text-surface-100"
      >
        <Plug size={14} aria-hidden />
        Your connections
      </h2>
      <p className="text-xs text-surface-600 dark:text-surface-400">
        These services see who is asking, so each person connects their own key. Yours is sent only
        with conversations you start, and the service decides what it lets you see. It is sealed in
        this browser so only the gateway can read it.
      </p>
      {!listed.seal_key && (
        <p className="text-xs text-amber-700 dark:text-amber-400">
          This deployment cannot seal keys yet, so none can be connected.
        </p>
      )}
      <ul className="space-y-3">
        {listed.connections.map((c) => (
          <Row
            key={c.rule_id}
            connection={c}
            sealKey={listed.seal_key}
            workspaceId={workspaceId}
            userId={userId}
            onChanged={load}
          />
        ))}
      </ul>
    </section>
  )
}

function Row({
  connection,
  sealKey,
  workspaceId,
  userId,
  onChanged,
}: {
  connection: OwnConnection
  sealKey: Listed['seal_key']
  workspaceId: string
  userId: string
  onChanged: () => Promise<void>
}) {
  const connected = connection.connected !== null
  const [editing, setEditing] = useState(false)
  const [secret, setSecret] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function run(work: () => Promise<unknown>) {
    setBusy(true)
    setError(null)
    try {
      await work()
      await onChanged()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'that did not work')
    } finally {
      setBusy(false)
    }
  }

  const save = () => {
    if (!sealKey) return
    const value = secret
    setSecret('')
    void run(async () => {
      await connectOwn(connection, workspaceId, userId, sealKey, value)
      setEditing(false)
    })
  }

  const remove = () => {
    if (!window.confirm(`Disconnect your key for ${connection.host}?`)) return
    void run(() => disconnectOwn(connection.rule_id))
  }

  return (
    <li className="rounded-md border border-surface-200 dark:border-surface-800 px-3 py-3 text-sm">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <span className="min-w-0 break-all font-mono text-surface-900 dark:text-surface-100">
          {connection.host}
        </span>
        {connected ? (
          <span className="ml-auto flex items-center gap-1 text-xs text-green-700 dark:text-green-400">
            <KeyRound size={12} aria-hidden />
            Connected {new Date(connection.connected!.connected_at).toLocaleDateString()}
          </span>
        ) : (
          <span className="ml-auto text-xs text-surface-500 dark:text-surface-400">Not connected</span>
        )}
      </div>

      {editing ? (
        <form
          className="mt-3 space-y-2"
          onSubmit={(e) => {
            e.preventDefault()
            save()
          }}
        >
          <label className="block text-xs text-surface-700 dark:text-surface-300">
            Your key, sent in <span className="font-mono">{connection.header}</span>
            <input
              type="password"
              autoComplete="off"
              value={secret}
              onChange={(e) => setSecret(e.target.value)}
              placeholder={connection.header === 'authorization' ? 'Bearer …' : ''}
              className="mt-1 w-full rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 px-2 py-1 font-mono text-xs"
            />
            <span className="mt-1 block text-surface-500 dark:text-surface-400">
              The whole header value, exactly as the service wants it.
            </span>
          </label>
          <div className="flex gap-2">
            <button
              type="submit"
              disabled={busy || secret === ''}
              className="rounded-md bg-brand-600 px-3 py-1.5 text-xs font-medium text-white disabled:opacity-50"
            >
              {connected ? 'Replace key' : 'Connect'}
            </button>
            <button
              type="button"
              onClick={() => setEditing(false)}
              className="rounded-md border border-surface-300 dark:border-surface-700 px-3 py-1.5 text-xs"
            >
              Cancel
            </button>
          </div>
        </form>
      ) : (
        <div className="mt-2 flex gap-2">
          <button
            type="button"
            onClick={() => setEditing(true)}
            disabled={!sealKey}
            className="rounded-md border border-surface-300 dark:border-surface-700 px-3 py-1.5 text-xs disabled:opacity-50"
          >
            {connected ? 'Replace key' : 'Connect your key'}
          </button>
          {connected && (
            <button
              type="button"
              onClick={remove}
              disabled={busy}
              className="rounded-md border border-red-300 dark:border-red-900 px-3 py-1.5 text-xs text-red-700 dark:text-red-400"
            >
              Disconnect
            </button>
          )}
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
