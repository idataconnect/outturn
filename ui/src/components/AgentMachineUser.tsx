import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { Bot } from 'lucide-react'

import { ApiError } from '../lib/api'
import {
  agentMachineUser,
  listMachineUsers,
  setAgentMachineUser,
  type MachineUser,
} from '../lib/credentials'
import { paths } from '../lib/paths'
import { useSession } from '../lib/session'

/**
 * Which machine user this agent's turns nobody started carry, if any.
 *
 * A schedule or a webhook starts a turn with nobody behind it; on a host whose
 * connection is each person's own, such a turn sends the key connected for
 * the machine user named here, or cannot reach the host. See
 * docs/user-credentials.md, *Nobody asked*.
 *
 * Shown only to somebody who may see the workspace's keys, and only once a
 * machine user exists: before that there is nothing to choose.
 */
export default function AgentMachineUser({ agentId }: { agentId: string }) {
  const state = useSession()
  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canSee = authorities.includes('credentials:read')
  const canEdit = authorities.includes('agents:update') && authorities.includes('credentials:write')

  const [machineUsers, setMachineUsers] = useState<MachineUser[] | null>(null)
  const [chosen, setChosen] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!canSee) return
    void (async () => {
      try {
        const [listed, current] = await Promise.all([listMachineUsers(), agentMachineUser(agentId)])
        setMachineUsers(listed.machine_users)
        setChosen(current.machine_user_id)
      } catch (e) {
        setError(e instanceof ApiError ? e.message : 'failed to load machine users')
      }
    })()
  }, [agentId, canSee])

  if (!canSee || machineUsers === null || (machineUsers.length === 0 && chosen === null)) {
    return error ? (
      <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
        {error}
      </p>
    ) : null
  }

  async function choose(id: string | null) {
    try {
      const set = await setAgentMachineUser(agentId, id)
      setChosen(set.machine_user_id)
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to choose a machine user')
    }
  }

  return (
    <section className="mt-8 space-y-2" aria-labelledby="agent-machine-user">
      <h2
        id="agent-machine-user"
        className="flex items-center gap-2 text-lg font-semibold text-surface-900 dark:text-surface-100"
      >
        <Bot size={18} aria-hidden />
        When nobody started the turn
      </h2>
      <p className="text-sm text-surface-600 dark:text-surface-400">
        On hosts where each person connects their own key, a turn a schedule or webhook started has
        nobody&apos;s key to send. It can send a machine user&apos;s instead; their keys are connected
        under <Link to={paths.connections} className="underline">Connections</Link>.
      </p>
      <label className="block max-w-sm text-sm text-surface-700 dark:text-surface-300">
        Machine user
        <select
          value={chosen ?? ''}
          disabled={!canEdit}
          onChange={(e) => void choose(e.target.value === '' ? null : e.target.value)}
          className="mt-1 w-full rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 px-3 py-2"
        >
          <option value="">Nobody — those turns cannot reach such hosts</option>
          {machineUsers.map((m) => (
            <option key={m.id} value={m.id}>
              {m.display_name}
            </option>
          ))}
        </select>
      </label>
      {error && (
        <p className="text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}
    </section>
  )
}
