import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { AlertTriangle, BookText, Building2, FileUp, GitFork, Layers } from 'lucide-react'

import { Badge, FilterBox, PageHeader, RecordList, RecordRow } from '../components/IndexPage'
import { ApiError } from '../lib/api'
import { matchesFilter } from '../lib/filter'
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
  const [query, setQuery] = useState('')

  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canWrite = authorities.includes('skills:write')
  const workspaceId = state.status === 'authenticated' ? state.session.workspace_id : null
  const isOperator =
    state.status === 'authenticated' && state.session.roles.includes('system_admin')

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

  const shown = skills.filter((s) => matchesFilter(query, s.name, s.slug, s.description))

  return (
    <div className="p-6 max-w-3xl">
      <PageHeader
        title="Skills"
        description="Instructions an agent is given beside its own prompt. The operator's reach every workspace; yours are your own, and can vary theirs without editing it."
        action={canWrite ? { to: '/skills/new', label: 'New skill' } : undefined}
      />

      {canWrite && isOperator && (
        <Link
          to="/skills/import"
          className="mt-3 inline-flex items-center gap-1.5 text-sm text-surface-700 dark:text-surface-300 underline underline-offset-2"
        >
          <FileUp size={14} aria-hidden />
          Import from OpenAPI
        </Link>
      )}

      {error && (
        <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      {skills.length > 0 && <FilterBox value={query} onChange={setQuery} label="Filter skills" />}
      <RecordList
        loading={loading}
        count={skills.length}
        shown={shown.length}
        empty={
          <>
            No skills yet.
            {canWrite && (
              <>
                {' '}
                <Link to="/skills/new" className="underline underline-offset-2">
                  Write one.
                </Link>
              </>
            )}
          </>
        }
      >
        {shown.map((skill) => {
          const operators = skill.workspace_id !== workspaceId
          const icon = skill.kind === 'override' ? Layers : operators ? Building2 : BookText
          return (
            <RecordRow
              key={skill.id}
              to={`/skills/${skill.id}`}
              icon={icon}
              title={skill.name}
              badges={
                <>
                  {operators && (
                    <span className="ml-2 align-middle text-xs font-normal px-1.5 py-0.5 rounded bg-surface-100 dark:bg-surface-800 text-surface-600 dark:text-surface-400">
                      from the operator
                    </span>
                  )}
                  {skill.retired_at && <Badge>retired</Badge>}
                </>
              }
            >
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
            </RecordRow>
          )
        })}
      </RecordList>
    </div>
  )
}
