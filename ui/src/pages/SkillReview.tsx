import { useCallback, useEffect, useState } from 'react'
import { Link, useParams } from 'react-router'
import { ArrowLeft } from 'lucide-react'

import SuggestedApprovals from '../components/SuggestedApprovals'
import { ApiError } from '../lib/api'
import { annotate, getSkill, getSource, type Skill, type SkillSource } from '../lib/skills'

/**
 * Confirming what an import guessed about each operation, as a step of its
 * own rather than a section at the foot of the skill's source: a real
 * specification has hundreds of operations, and confirming them is the work.
 */
export default function SkillReview() {
  const { id = '' } = useParams()
  const [skill, setSkill] = useState<Skill | null>(null)
  const [source, setSource] = useState<SkillSource | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null)

  const load = useCallback(async () => {
    const [s, src] = await Promise.all([getSkill(id), getSource(id)])
    setSkill(s)
    setSource(src)
  }, [id])

  useEffect(() => {
    load().catch((e) => setError(e instanceof ApiError ? e.message : 'failed to load the skill'))
  }, [load])

  // One rule at a time, so the count moves while a long group is written and
  // a failure part way says how far it got; what was written stays written.
  async function confirm(rules: { target: string; value: string }[]) {
    setError(null)
    setProgress({ done: 0, total: rules.length })
    let done = 0
    try {
      for (const r of rules) {
        await annotate(id, { level: 'operation', target: r.target, kind: 'approval', value: r.value })
        done++
        setProgress({ done, total: rules.length })
      }
    } catch (e) {
      setError(
        `${done} of ${rules.length} written, then: ${e instanceof ApiError ? e.message : String(e)}`,
      )
    } finally {
      setProgress(null)
      await load().catch(() => {})
    }
  }

  const operations = source?.outline?.operations ?? []
  return (
    <div className="mx-auto max-w-3xl p-4 sm:p-6">
      <Link
        to={`/skills/${id}`}
        className="inline-flex items-center gap-1 text-sm text-surface-600 dark:text-surface-400"
      >
        <ArrowLeft size={14} aria-hidden /> {skill?.name ?? 'The skill'}
      </Link>
      <h1 className="mt-3 text-2xl font-semibold text-surface-900 dark:text-surface-100">
        Confirm how risky each operation is
      </h1>
      <p className="mt-2 text-sm text-surface-600 dark:text-surface-400">
        When this skill was imported, each operation was given a guess, from its method and name, at
        whether an agent should ask before calling it and how serious a call is. How serious decides
        who is asked, and what an auto-approval policy may let through without asking anybody. They
        are only guesses, and none applies until it is confirmed here.
      </p>
      {error && (
        <p role="alert" className="mt-3 text-sm text-red-600 dark:text-red-400">
          {error}
        </p>
      )}
      <div className="mt-4">
        {source && operations.some((o) => o.suggested_approval) ? (
          <SuggestedApprovals
            operations={operations}
            annotations={source.annotations}
            baseUrl={source.base_url}
            busy={progress !== null}
            progress={progress}
            onConfirm={(rules) => void confirm(rules)}
          />
        ) : (
          source && <p className="text-sm text-surface-500">Nothing was guessed for this skill.</p>
        )}
      </div>
    </div>
  )
}
