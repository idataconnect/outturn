import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { AlertTriangle, Clock, Webhook as WebhookIcon } from 'lucide-react'

import { allPages } from '../lib/api'
import { listSchedules } from '../lib/schedules'
import { fromSchedule, fromWebhook, ordered, type Trigger } from '../lib/triggers'
import { type Webhook } from '../lib/webhooks'

const SHOWN = 6

/**
 * The workspace's schedules and webhooks: what runs without anybody asking.
 *
 * On the dashboard because each lives on its agent's page, which is the right
 * place to edit one and the wrong place to notice that one stopped working --
 * a schedule failing every morning is a broken integration nobody is
 * watching until somebody opens that particular agent.
 */
export default function RunningOnTheirOwn() {
  const [triggers, setTriggers] = useState<Trigger[] | null>(null)
  const [agents, setAgents] = useState<Map<string, string>>(new Map())

  useEffect(() => {
    let current = true
    void Promise.all([
      listSchedules(),
      allPages<Webhook>('/v1/webhook-triggers'),
      allPages<{ id: string; name: string }>('/v1/agents'),
    ])
      .then(([schedules, webhooks, list]) => {
        if (!current) return
        setTriggers(ordered([...schedules.map(fromSchedule), ...webhooks.map(fromWebhook)]))
        setAgents(new Map(list.map((a) => [a.id, a.name])))
      })
      // Not this page's error to show: the panel is a convenience over the
      // agents' own pages, and a failure to read it leaves nothing wrong there.
      .catch(() => {
        if (current) setTriggers(null)
      })
    return () => {
      current = false
    }
  }, [])

  if (!triggers) return null

  const troubled = triggers.filter((t) => t.trouble).length
  const schedules = triggers.filter((t) => t.kind === 'schedule').length
  const webhooks = triggers.length - schedules
  const counted = [
    schedules && `${schedules} schedule${schedules === 1 ? '' : 's'}`,
    webhooks && `${webhooks} webhook${webhooks === 1 ? '' : 's'}`,
  ]
    .filter(Boolean)
    .join(', ')

  return (
    <section
      aria-labelledby="running-on-their-own"
      className="rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 p-5"
    >
      <h2
        id="running-on-their-own"
        className="text-xs font-medium uppercase tracking-wide text-surface-500 dark:text-surface-400"
      >
        Running on their own
      </h2>
      {triggers.length === 0 ? (
        <p className="mt-2 text-sm text-surface-600 dark:text-surface-400">
          No schedules or webhooks. Each agent&rsquo;s settings page is where they are made.
        </p>
      ) : (
        <>
          <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
            {counted}
            {troubled > 0 && (
              <span className="text-red-600 dark:text-red-400"> · {troubled} not working</span>
            )}
          </p>
          <ul className="mt-3 divide-y divide-surface-100 dark:divide-surface-800">
            {triggers.slice(0, SHOWN).map((t) => {
              const Icon = t.kind === 'schedule' ? Clock : WebhookIcon
              return (
                <li key={`${t.kind}:${t.id}`}>
                  <Link
                    to={`/agents/${t.agentId}/edit`}
                    className="flex items-start gap-3 py-2 -mx-2 px-2 rounded hover:bg-surface-50 dark:hover:bg-surface-800/50"
                  >
                    {t.trouble ? (
                      <AlertTriangle
                        className="w-4 h-4 mt-0.5 shrink-0 text-red-600 dark:text-red-400"
                        aria-label="Not working"
                      />
                    ) : (
                      <Icon
                        className={`w-4 h-4 mt-0.5 shrink-0 ${t.enabled ? 'text-surface-500' : 'text-surface-300 dark:text-surface-600'}`}
                        aria-label={t.kind === 'schedule' ? 'Schedule' : 'Webhook'}
                      />
                    )}
                    <span className="min-w-0 flex-1">
                      <span className="flex items-baseline justify-between gap-3">
                        <span
                          className={`truncate text-sm ${t.enabled ? 'text-surface-900 dark:text-surface-100' : 'text-surface-500'}`}
                        >
                          {t.name}
                          <span className="text-surface-500"> · {agents.get(t.agentId) ?? 'an agent'}</span>
                        </span>
                        <span className="shrink-0 text-xs text-surface-500">{t.status}</span>
                      </span>
                      {t.trouble && (
                        <span className="block truncate text-xs text-red-600 dark:text-red-400">
                          {t.trouble}
                        </span>
                      )}
                    </span>
                  </Link>
                </li>
              )
            })}
          </ul>
          {triggers.length > SHOWN && (
            <p className="mt-2 text-xs text-surface-500">
              And {triggers.length - SHOWN} more, on their agents&rsquo; pages.
            </p>
          )}
        </>
      )}
    </section>
  )
}
