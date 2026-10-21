import { useEffect, useState } from 'react'
import { ShieldQuestion } from 'lucide-react'

import { answerApproval } from '../lib/actions'
import { OFFER_AFTER, answeredCount, policyLink } from '../lib/autoApproval'
import { useHasAuthority } from '../lib/session'

/**
 * An approval, answered where it was raised.
 *
 * The queue is the place every pending decision can be found, across every
 * workspace somebody belongs to. This is the other half: the person who was
 * talking to the agent when it stopped is usually the person who can let it
 * carry on, and sending them to a different screen to do it loses the thread of
 * what they were doing.
 *
 * Shown only when the held event named one, so a conversation paused by a spend
 * cap or an operator gets nothing here -- neither is answerable, and offering a
 * button that cannot work is worse than offering none.
 *
 * Shown only to somebody who may answer, which the caller decides from the
 * authorities their session already carries. Rendering it for everyone was
 * tried: a clerk watching their own conversation was offered Approve and told
 * 403 by the API, which is an invitation to do something they cannot. The API
 * check stays where it is -- this narrows what is offered, never what is
 * allowed.
 */
/** One field the approval binds, and the value this request gave it. */
export type Bound = { field: string; value: unknown }

/** A bound value as text: missing said so, anything structured as JSON. */
function shown(value: unknown): string {
  if (value === null || value === undefined) return 'missing'
  if (typeof value === 'string') return value
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  return JSON.stringify(value)
}

export default function ApprovalPrompt({
  approval,
  onAnswered,
}: {
  approval: {
    item_id: string
    requires?: string | null
    reason?: string | null
    /** What is being approved: each field the grant is keyed on, with this
     *  request's value. */
    binds?: Bound[] | null
  }
  onAnswered: () => void
}) {
  const [busy, setBusy] = useState(false)
  const [failed, setFailed] = useState<string | null>(null)

  // A workspace that keeps saying yes to the same act is offered a policy for
  // it -- a link, never a box ticked on the way past, since a policy is its own
  // decision with its own page. Only to somebody who could set one: the count
  // is asked for only then, and the API says again whether they may.
  const delegates = useHasAuthority('approvals:delegate')
  const answers = useHasAuthority('approvals:answer')
  const mayDelegate = delegates && answers
  const [offer, setOffer] = useState<string | null>(null)
  const requires = approval.requires
  const fields = (approval.binds ?? []).map((b) => b.field).join(',')
  useEffect(() => {
    if (!mayDelegate || !requires) return
    let stale = false
    answeredCount(requires)
      .then((n) => {
        if (!stale && n.may_delegate && n.approved >= OFFER_AFTER) {
          setOffer(policyLink(requires, fields ? fields.split(',') : []))
        }
      })
      .catch(() => {})
    return () => {
      stale = true
    }
  }, [mayDelegate, requires, fields])

  async function answer(approved: boolean) {
    setBusy(true)
    setFailed(null)
    try {
      await answerApproval(approval.item_id, { approved })
      onAnswered()
    } catch (e) {
      // Left on screen rather than swallowed: the turn is still held, and a
      // button that appeared to do nothing is how somebody concludes the
      // conversation is broken.
      setFailed(e instanceof Error ? e.message : 'that could not be sent')
      setBusy(false)
    }
  }

  return (
    <div className="my-3 rounded-md border border-amber-300 bg-amber-50 p-3 dark:border-amber-900 dark:bg-amber-950/40">
      <div className="flex items-start gap-3">
        <ShieldQuestion
          size={16}
          aria-hidden
          className="mt-0.5 shrink-0 text-amber-600 dark:text-amber-400"
        />
        <div className="min-w-0 flex-1">
          <p className="text-sm text-surface-900 dark:text-surface-100">
            <span className="font-medium">
              {approval.requires ? `This needs approval: ${approval.requires}` : 'This needs approval'}
            </span>
          </p>
          {approval.reason && (
            <p className="mt-0.5 text-xs text-surface-700 dark:text-surface-300">
              {approval.reason}
            </p>
          )}

          {/* The values themselves, exactly as the request carries them. A yes
              is a yes to these -- the grant is keyed on them -- so they are
              shown as they are rather than as this page guesses they mean: it
              cannot know that a field is pence, and a rounded or converted
              figure would be approving something the request does not say. */}
          {approval.binds && approval.binds.length > 0 && (
            <dl className="mt-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 text-xs">
              {approval.binds.map(({ field, value }) => (
                <div key={field} className="contents">
                  <dt className="font-mono text-surface-600 dark:text-surface-400">{field}</dt>
                  <dd
                    className={`font-mono break-all ${
                      value === null || value === undefined
                        ? 'text-red-700 dark:text-red-400'
                        : 'text-surface-900 dark:text-surface-100'
                    }`}
                  >
                    {shown(value)}
                  </dd>
                </div>
              ))}
            </dl>
          )}

          <div className="mt-2 flex flex-wrap gap-2">
            <button
              type="button"
              disabled={busy}
              onClick={() => answer(true)}
              className="rounded-md bg-brand-600 px-2.5 py-1 text-xs font-medium text-white hover:bg-brand-700 disabled:opacity-50"
            >
              {busy ? 'Sending…' : 'Approve'}
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={() => answer(false)}
              className="rounded-md border border-surface-300 px-2.5 py-1 text-xs font-medium text-surface-700 hover:bg-surface-50 disabled:opacity-50 dark:border-surface-700 dark:text-surface-300 dark:hover:bg-surface-800"
            >
              Decline
            </button>
          </div>

          {offer && (
            <p className="mt-2 text-xs text-surface-600 dark:text-surface-400">
              <a href={offer} className="text-brand-700 hover:underline dark:text-brand-300">
                Auto-approval policy for “{requires}”
              </a>{' '}
              · this workspace has approved it several times before
            </p>
          )}

          {failed && (
            // Announced, because the person who pressed Approve is the one who
            // needs to know it did not go: the surrounding banner is a `status`
            // deliberately -- the state of the conversation -- and a failure to
            // answer is not that.
            <p role="alert" className="mt-2 text-xs text-red-700 dark:text-red-400">
              {failed}
            </p>
          )}
        </div>
      </div>
    </div>
  )
}
