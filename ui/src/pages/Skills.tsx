import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { AlertTriangle, BookText, Building2, GitFork, Layers, Plus } from 'lucide-react'

import { ApiError } from '../lib/api'
import { listSkills, type Skill } from '../lib/skills'
import { useSession } from '../lib/session'

/**
 * Every skill this workspace can reach: its own, and the operator's.
 *
 * The operator's are listed beside the workspace's rather than behind a tab,
 * because the decision a reader is here to make -- do I need to vary this? --
 * is about the whole set, and a skill they cannot see is one they cannot
 * decide about.
 */
export default function Skills() {
  const state = useSession()
  const [skills, setSkills] = useState<Skill[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canWrite = authorities.includes('skills:write')
  const workspaceId = state.status === 'authenticated' ? state.session.workspace_id : null

  useEffect(() => {
    void (async () => {
      try {
        setSkills(await listSkills())
        setError(null)
      } catch (e) {
        setError(e instanceof ApiError ? e.message : 'failed to load skills')
      } finally {
        setLoading(false)
      }
    })()
  }, [workspaceId])

  const name = (id: string | null) => skills.find((s) => s.id === id)?.name ?? 'another skill'

  return (
    <div className="p-6 max-w-3xl">
      <div className="flex items-start justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold text-surface-900 dark:text-surface-100">Skills</h1>
          <p className="mt-2 text-surface-600 dark:text-surface-400">
            Prose an agent is given beside its own prompt. The operator's reach every
            workspace; yours are your own, and can vary theirs without editing it.
          </p>
        </div>
        {canWrite && (
          <Link
            to="/skills/new"
            className="shrink-0 flex items-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium"
          >
            <Plus size={16} aria-hidden />
            New skill
          </Link>
        )}
      </div>

      {error && (
        <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      <div className="mt-6 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 overflow-hidden">
        {loading ? (
          <p className="p-4 text-sm text-surface-600 dark:text-surface-400">Loading…</p>
        ) : skills.length === 0 ? (
          <p className="p-4 text-sm text-surface-600 dark:text-surface-400">
            No skills yet.
            {canWrite && (
              <>
                {' '}
                <Link to="/skills/new" className="underline underline-offset-2">
                  Write one.
                </Link>
              </>
            )}
          </p>
        ) : (
          <ul className="divide-y divide-surface-200 dark:divide-surface-800">
            {skills.map((skill) => {
              const operators = skill.workspace_id !== workspaceId
              const Icon = skill.kind === 'override' ? Layers : operators ? Building2 : BookText
              return (
                <li key={skill.id} className="flex items-start gap-4 p-4">
                  <Icon size={16} className="mt-1 shrink-0 text-surface-400" aria-hidden />
                  <Link to={`/skills/${skill.id}`} className="flex-1 min-w-0 group">
                    <p className="text-sm font-medium text-surface-900 dark:text-surface-100 truncate group-hover:underline underline-offset-2">
                      {skill.name}
                      {operators && (
                        <span className="ml-2 align-middle text-xs font-normal px-1.5 py-0.5 rounded bg-surface-100 dark:bg-surface-800 text-surface-600 dark:text-surface-400">
                          from the operator
                        </span>
                      )}
                      {skill.retired_at && (
                        <span className="ml-2 text-xs font-normal text-surface-600 dark:text-surface-400">
                          retired
                        </span>
                      )}
                    </p>
                    <p className="text-xs font-mono text-surface-600 dark:text-surface-400 truncate">
                      {skill.slug}
                      {skill.ordinal !== null && ` · v${skill.ordinal}`}
                    </p>
                    {skill.kind === 'override' && (
                      <p className="mt-1 text-xs text-surface-600 dark:text-surface-400">
                        Varies {name(skill.base_skill_id)} wherever it is used.
                      </p>
                    )}
                    {skill.forked_from_skill_id && (
                      <p className="mt-1 flex items-center gap-1 text-xs text-surface-600 dark:text-surface-400">
                        <GitFork size={12} aria-hidden />
                        Forked from {name(skill.forked_from_skill_id)}
                      </p>
                    )}
                    {skill.description && (
                      <p className="mt-1 text-xs text-surface-600 dark:text-surface-400 line-clamp-2">
                        {skill.description}
                      </p>
                    )}
                    {skill.base_moved && (
                      <p className="mt-1 flex items-center gap-1 text-xs text-amber-700 dark:text-amber-500">
                        <AlertTriangle size={12} aria-hidden />
                        {name(skill.base_skill_id)} has changed since this was written.
                      </p>
                    )}
                  </Link>
                </li>
              )
            })}
          </ul>
        )}
      </div>
    </div>
  )
}
