import { useEffect, useState } from 'react'
import { ShieldCheck } from 'lucide-react'

import ApprovalBands from '../components/ApprovalBands'
import HighRiskRoute from '../components/HighRiskRoute'
import { Badge, PageHeader, RecordList, RecordRow } from '../components/IndexPage'
import { ApiError } from '../lib/api'
import {
  covers,
  dailyTotals,
  decimalsOf,
  listBands,
  listPolicies,
  stateLabel,
  type BandsPage,
  type Policy,
} from '../lib/autoApproval'
import { paths } from '../lib/paths'
import { useSession } from '../lib/session'

/**
 * The workspace's auto-approval policies: which gated requests go out without
 * a person, each naming who set it. Readable by whoever answers approvals,
 * since what is waved through is what an approver most needs to know; written
 * by whoever may also delegate.
 */
export default function AutoApproval() {
  const state = useSession()
  const [policies, setPolicies] = useState<Policy[]>([])
  const [bands, setBands] = useState<BandsPage | null>(null)
  const decimals = decimalsOf(bands)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  // Moved on by anything here that writes, so the page reads again what it
  // changed: a band moves what every operation in its currency says.
  const [reloads, setReloads] = useState(0)
  const reload = () => setReloads((n) => n + 1)

  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canWrite = authorities.includes('approvals:delegate') && authorities.includes('approvals:answer')
  const workspaceId = state.status === 'authenticated' ? state.session.workspace_id : null

  useEffect(() => {
    let stale = false
    void (async () => {
      try {
        // The bands page is where a currency's decimals are, so a daily total
        // can be said as money; without it, a total is said in minor units.
        const [found, page] = await Promise.all([listPolicies(), listBands().catch(() => null)])
        if (stale) return
        setPolicies(found)
        setBands(page)
      } catch (e) {
        if (!stale) setError(e instanceof ApiError ? e.message : 'failed to load policies')
      } finally {
        if (!stale) setLoading(false)
      }
    })()
    return () => {
      stale = true
    }
  }, [workspaceId, reloads])

  return (
    <div className="p-6 max-w-3xl">
      <PageHeader
        title="Auto-approval"
        description="Gated requests that go out without asking a person, within the limits each policy sets. A new policy starts in trial: people are still asked, and each question says whether the policy would have approved it."
        action={canWrite ? { to: paths.newAutoApproval, label: 'New policy' } : undefined}
      />

      {error && (
        <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      <RecordList
        loading={loading}
        count={policies.length}
        shown={policies.length}
        empty="No policies. Every gated request asks a person."
      >
        {policies.map((p) => (
          <RecordRow
            key={p.id}
            to={paths.autoApproval(p.id)}
            icon={ShieldCheck}
            title={covers(p)}
            badges={
              <Badge>
                {p.effect === 'exclude' ? 'Exclusion' : stateLabel[p.state]}
                {p.agent_name ? `, ${p.agent_name} only` : ''}
              </Badge>
            }
          >
            <p className="mt-1 text-xs text-surface-600 dark:text-surface-400">
              Set by {p.created_by_name ?? 'somebody no longer here'}
              {p.state === 'revoked'
                ? `, revoked${p.revoked_by_name ? ` by ${p.revoked_by_name}` : ''}`
                : `, until ${new Date(p.expires_at).toLocaleDateString()}`}
              {p.effect === 'include' && ` · ${p.auto_approved} approved automatically`}
            </p>
            {dailyTotals(p.daily_totals, decimals).length > 0 && (
              <p className="mt-0.5 text-xs text-surface-600 dark:text-surface-400">
                Up to {dailyTotals(p.daily_totals, decimals).join(' and ')} a day
              </p>
            )}
          </RecordRow>
        ))}
      </RecordList>

      {bands && <ApprovalBands page={bands} onChanged={reload} />}
      <HighRiskRoute />
    </div>
  )
}
