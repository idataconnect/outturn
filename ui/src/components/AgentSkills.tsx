import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { AlertTriangle, Layers } from 'lucide-react'

import { ApiError } from '../lib/api'
import { agentSkills, listSkills, setAgentSkills, type Skill } from '../lib/skills'
import { useSession } from '../lib/session'

/**
 * Which skills this agent is given.
 *
 * Only skills proper are listed. An override is the workspace's standing
 * variation on one and is composed wherever its base is used, so offering it
 * here would be asking for the same thing twice -- it is shown against the
 * skill it varies instead, which is where a reader is deciding.
 */
export default function AgentSkills({ agentId }: { agentId: string }) {
  const state = useSession()
  const [skills, setSkills] = useState<Skill[]>([])
  const [chosen, setChosen] = useState<string[]>([])
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canEdit = authorities.includes('agents:update')
  const workspaceId = state.status === 'authenticated' ? state.session.workspace_id : null

  useEffect(() => {
    void (async () => {
      try {
        const [all, bound] = await Promise.all([listSkills(), agentSkills(agentId)])
        setSkills(all)
        setChosen(bound.map((b) => b.skill_id))
        setError(null)
      } catch (e) {
        setError(e instanceof ApiError ? e.message : 'failed to load skills')
      } finally {
        setLoading(false)
      }
    })()
  }, [agentId])

  async function toggle(id: string, on: boolean) {
    const next = on ? [...chosen, id] : chosen.filter((x) => x !== id)
    setChosen(next)
    setSaving(true)
    try {
      await setAgentSkills(
        agentId,
        next.map((skill_id, position) => ({ skill_id, version_id: null, position })),
      )
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to save skills')
      setChosen(chosen)
    } finally {
      setSaving(false)
    }
  }

  // Overrides are shown against what they vary, not as things to pick.
  const offered = skills.filter((s) => s.kind !== 'override' && s.retired_at === null)
  const overrideFor = (id: string) =>
    skills.find((s) => s.kind === 'override' && s.base_skill_id === id && !s.retired_at)

  if (loading) return null

  return (
    <section className="mt-8 space-y-4">
      <div>
        <h2 className="text-lg font-semibold text-surface-900 dark:text-surface-100">Skills</h2>
        <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
          Given to this agent beside its own prompt, in this order.{' '}
          {saving && <span className="text-surface-500">Saving…</span>}
        </p>
      </div>

      {error && (
        <p className="text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      {offered.length === 0 ? (
        <p className="text-sm text-surface-600 dark:text-surface-400">
          No skills yet.{' '}
          <Link to="/skills/new" className="underline underline-offset-2">
            Write one.
          </Link>
        </p>
      ) : (
        <ul className="divide-y divide-surface-200 dark:divide-surface-800 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 overflow-hidden">
          {offered.map((skill) => {
            const variation = overrideFor(skill.id)
            return (
              <li key={skill.id} className="flex items-start gap-3 p-4">
                <input
                  type="checkbox"
                  id={`skill-${skill.id}`}
                  checked={chosen.includes(skill.id)}
                  disabled={!canEdit}
                  onChange={(e) => void toggle(skill.id, e.target.checked)}
                  className="mt-1"
                />
                <div className="flex-1 min-w-0">
                  <label
                    htmlFor={`skill-${skill.id}`}
                    className="text-sm font-medium text-surface-900 dark:text-surface-100"
                  >
                    {skill.name}
                    {skill.workspace_id !== workspaceId && (
                      <span className="ml-2 text-xs font-normal px-1.5 py-0.5 rounded bg-surface-100 dark:bg-surface-800 text-surface-600 dark:text-surface-400">
                        from the operator
                      </span>
                    )}
                  </label>
                  {skill.description && (
                    <p className="text-xs text-surface-600 dark:text-surface-400">
                      {skill.description}
                    </p>
                  )}
                  {variation && chosen.includes(skill.id) && (
                    <p className="mt-1 flex items-center gap-1 text-xs text-surface-600 dark:text-surface-400">
                      <Layers size={12} aria-hidden />
                      <Link to={`/skills/${variation.id}`} className="underline underline-offset-2">
                        {variation.name}
                      </Link>{' '}
                      applies after it.
                      {variation.base_moved && (
                        <span className="ml-1 inline-flex items-center gap-1 text-amber-700 dark:text-amber-500">
                          <AlertTriangle size={12} aria-hidden />
                          needs review
                        </span>
                      )}
                    </p>
                  )}
                </div>
              </li>
            )
          })}
        </ul>
      )}
    </section>
  )
}
