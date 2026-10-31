import { useCallback, useEffect, useState } from 'react'
import { Bot, Plus, Trash2 } from 'lucide-react'

import { ApiError } from '../lib/api'
import {
  connectMachine,
  createMachineUser,
  deleteMachineUser,
  disconnectMachine,
  listMachineUsers,
  type MachineUser,
  type OwnConnections,
} from '../lib/credentials'
import { KeyRow } from './OwnConnections'

/**
 * Accounts that hold keys for the turns nobody sent.
 *
 * A schedule or a webhook starts a turn with nobody behind it, so a host whose
 * connection is each person's own has nobody's key to send. A machine user is
 * a caller of its own for those turns: an administrator makes one, connects
 * its keys here, and names it on an agent. Nobody signs in as it and it holds
 * no role, which is why its keys are connected by an administrator when a
 * person's are theirs alone. See docs/user-credentials.md, *Nobody asked*.
 *
 * Shown only where some host's connection is each person's own: elsewhere
 * there is nothing for one to hold.
 */
export default function MachineUsers({
  workspaceId,
  askers,
  reloads,
}: {
  workspaceId: string
  /** How many hosts' connections are each person's own. */
  askers: number
  /** Bumped by the page when a host changes, so the keys listed follow. */
  reloads: number
}) {
  const [listed, setListed] = useState<{
    machine_users: MachineUser[]
    seal_key: OwnConnections['seal_key']
  } | null>(null)
  const [name, setName] = useState('')
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(async () => {
    try {
      setListed(await listMachineUsers())
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to load machine users')
    }
  }, [])

  useEffect(() => {
    void load()
  }, [load, reloads])

  async function act(work: () => Promise<unknown>) {
    try {
      await work()
      await load()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'that did not work')
    }
  }

  if (askers === 0 && (listed?.machine_users.length ?? 0) === 0) return null

  return (
    <section className="mt-8" aria-labelledby="machine-users">
      <h2
        id="machine-users"
        className="flex items-center gap-2 text-sm font-medium text-surface-800 dark:text-surface-200"
      >
        <Bot size={14} aria-hidden />
        Machine users
      </h2>
      <p className="mt-1 text-xs text-surface-600 dark:text-surface-400">
        A caller of its own for turns nobody started — a schedule, a webhook — on hosts where each
        person connects their own key. Make one, connect its keys here, and choose it on an agent.
        Nobody signs in as it.
      </p>
      {error && (
        <p className="mt-2 text-xs text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      <form
        className="mt-3 flex gap-2"
        onSubmit={(e) => {
          e.preventDefault()
          const n = name.trim()
          if (!n) return
          setName('')
          void act(() => createMachineUser(n))
        }}
      >
        <input
          aria-label="Machine user name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Nightly reports"
          className="flex-1 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 px-3 py-1.5 text-sm"
        />
        <button
          type="submit"
          disabled={!name.trim()}
          className="flex items-center gap-1 rounded-md border border-surface-300 dark:border-surface-700 px-3 py-1.5 text-sm disabled:opacity-50"
        >
          <Plus size={14} aria-hidden />
          Add machine user
        </button>
      </form>

      <ul className="mt-3 space-y-4">
        {(listed?.machine_users ?? []).map((m) => (
          <li
            key={m.id}
            className="rounded-md border border-surface-200 dark:border-surface-800 px-3 py-3"
          >
            <div className="flex items-center gap-2">
              <span className="text-sm font-medium text-surface-900 dark:text-surface-100">
                {m.display_name}
              </span>
              <span className="text-xs text-surface-500 dark:text-surface-400">
                {m.agents.length === 0
                  ? 'No agent uses it yet'
                  : `Used by ${m.agents.length} agent${m.agents.length === 1 ? '' : 's'}`}
              </span>
              <button
                type="button"
                aria-label={`Delete ${m.display_name}`}
                title="Delete it and every key connected for it"
                onClick={() => {
                  if (
                    window.confirm(
                      `Delete ${m.display_name}? Its keys are revoked, and agents using it have nobody for turns nobody started.`,
                    )
                  ) {
                    void act(() => deleteMachineUser(m.id))
                  }
                }}
                className="ml-auto rounded p-1 text-surface-400 hover:text-red-600"
              >
                <Trash2 size={14} aria-hidden />
              </button>
            </div>
            {m.connections.length === 0 ? (
              <p className="mt-2 text-xs text-surface-500 dark:text-surface-400">
                No host&apos;s connection is each person&apos;s own yet, so it has nothing to hold.
              </p>
            ) : (
              <ul className="mt-2 space-y-2">
                {m.connections.map((c) => (
                  <KeyRow
                    key={c.rule_id}
                    connection={c}
                    sealable={listed?.seal_key != null}
                    whose={m.display_name}
                    onConnect={(secret) =>
                      connectMachine(m.id, c, workspaceId, listed!.seal_key!, secret)
                    }
                    onDisconnect={() => disconnectMachine(m.id, c.rule_id)}
                    onChanged={load}
                  />
                ))}
              </ul>
            )}
          </li>
        ))}
      </ul>
    </section>
  )
}
