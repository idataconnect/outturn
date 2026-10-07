import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { Plus } from 'lucide-react'

import { ApiError } from '../lib/api'
import { AVAILABILITY, listTemplates, type Template } from '../lib/agentTemplates'
import { paths } from '../lib/paths'

/**
 * The operator's agent templates: agents defined once and made in every
 * workspace that should have them. See docs/agent-templates.md.
 */
export default function AgentTemplates() {
  const [templates, setTemplates] = useState<Template[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let stale = false
    listTemplates().then(
      (list) => {
        if (stale) return
        setTemplates(list)
        setLoading(false)
      },
      (e) => {
        if (stale) return
        setError(e instanceof ApiError ? e.message : 'failed to load templates')
        setLoading(false)
      },
    )
    return () => {
      stale = true
    }
  }, [])

  return (
    <div className="p-6 max-w-3xl">
      <div className="flex items-start justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold text-surface-900 dark:text-surface-100">
            Agent templates
          </h1>
          <p className="mt-2 text-surface-600 dark:text-surface-400">
            Agents defined once and made in every workspace that should have them. Each
            workspace&rsquo;s agent follows its template&rsquo;s newest version.
          </p>
        </div>
        <Link
          to={paths.agentTemplateNew}
          className="shrink-0 flex items-center gap-2 px-3 py-1.5 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium"
        >
          <Plus size={14} aria-hidden />
          New template
        </Link>
      </div>

      {error && (
        <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      {loading ? (
        <p className="mt-6 text-sm text-surface-600 dark:text-surface-400">Loading…</p>
      ) : templates.length === 0 ? (
        <p className="mt-6 text-sm text-surface-600 dark:text-surface-400">No templates yet.</p>
      ) : (
        <ul className="mt-6 divide-y divide-surface-200 dark:divide-surface-800 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 overflow-hidden">
          {templates.map((t) => (
            <li key={t.id}>
              <Link
                to={paths.agentTemplate(t.id)}
                className="block p-4 hover:bg-surface-50 dark:hover:bg-surface-800/50"
              >
                <p className="text-sm font-medium text-surface-900 dark:text-surface-100">
                  {t.current.name}
                  <span className="ml-2 text-xs font-normal text-surface-500 dark:text-surface-400">
                    {AVAILABILITY[t.availability].label} · v{t.current.ordinal}
                    {t.retired && ' · retired'}
                  </span>
                </p>
                {t.current.description && (
                  <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
                    {t.current.description}
                  </p>
                )}
              </Link>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
