import { useState } from 'react'
import { Plus } from 'lucide-react'

import AgentSchedules from './AgentSchedules'
import AgentWebhooks from './AgentWebhooks'
import TriggerIcon from './TriggerIcon'
import { useSession } from '../lib/session'

type Kind = 'schedule' | 'webhook' | 'email'

/**
 * Everything that starts this agent's turns without a person: one section,
 * whatever the kind. Adding one asks which kind first, and the rest is that
 * kind's own form. See docs/triggers.md, "The API and the page".
 */
export default function AgentTriggers({ agentId }: { agentId: string }) {
  const state = useSession()
  const canEdit =
    state.status === 'authenticated' && state.session.authorities.includes('agents:update')
  const [choosing, setChoosing] = useState(false)
  const [creating, setCreating] = useState<Kind | null>(null)

  const start = (kind: Kind) => {
    setChoosing(false)
    setCreating(kind)
  }
  const done = () => setCreating(null)

  return (
    <section className="mt-8 space-y-4">
      <div className="flex items-baseline justify-between gap-4">
        <div>
          <h2 className="text-lg font-semibold text-surface-900 dark:text-surface-100">
            Triggers
          </h2>
          <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
            Turns this agent starts on its own: on a schedule, when another system calls its
            webhook, or when mail arrives. Nobody is waiting for the reply, so these run behind anything a person
            is watching.
          </p>
        </div>
        {canEdit && !creating && (
          <div className="relative shrink-0">
            <button
              type="button"
              aria-expanded={choosing}
              onClick={() => setChoosing((c) => !c)}
              className="inline-flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-md bg-brand-600 text-white hover:bg-brand-700"
            >
              <Plus className="w-4 h-4" /> New trigger
            </button>
            {choosing && (
              <div className="absolute right-0 z-10 mt-1 w-56 rounded-md border border-surface-200 dark:border-surface-700 bg-white dark:bg-surface-800 shadow-lg py-1">
                {(
                  [
                    ['schedule', 'Schedule', 'At set times'],
                    ['webhook', 'Webhook', 'When another system calls it'],
                    ['email', 'Email', 'When mail arrives'],
                  ] as const
                ).map(([kind, label, hint]) => (
                  <button
                    key={kind}
                    type="button"
                    onClick={() => start(kind)}
                    className="w-full text-left px-3 py-2 text-sm hover:bg-surface-100 dark:hover:bg-surface-700 flex items-start gap-2"
                  >
                    <TriggerIcon kind={kind} className="w-4 h-4 mt-0.5 text-surface-500" />
                    <span>
                      <span className="block text-surface-900 dark:text-surface-100">{label}</span>
                      <span className="block text-xs text-surface-500">{hint}</span>
                    </span>
                  </button>
                ))}
              </div>
            )}
          </div>
        )}
      </div>

      <AgentSchedules
        agentId={agentId}
        embedded={{ creating: creating === 'schedule', onDone: done }}
      />
      <AgentWebhooks
        agentId={agentId}
        embedded={{
          creating: creating === 'webhook' || creating === 'email',
          kind: creating === 'email' ? 'email' : 'webhook',
          onDone: done,
        }}
      />
    </section>
  )
}
