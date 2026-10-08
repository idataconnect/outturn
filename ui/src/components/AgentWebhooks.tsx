import { useCallback, useEffect, useState } from 'react'
import { AlertTriangle, Check, Copy, KeyRound, Plus, Trash2 } from 'lucide-react'

import { ApiError } from '../lib/api'
import {
  BODY,
  createWebhook,
  deleteWebhook,
  isClearlyPrivate,
  listWebhooks,
  rotateWebhookSecret,
  updateWebhook,
  webhookUrl,
  type Scheme,
  type Webhook,
  type WebhookInput,
} from '../lib/webhooks'
import { useSession } from '../lib/session'

const field =
  'w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100'
const label = 'block text-sm text-surface-700 dark:text-surface-300 mb-1'

const SCHEMES: { value: Scheme; label: string; detail: string }[] = [
  {
    value: 'hmac',
    label: 'Signed',
    detail:
      'The sender signs each body and its time with the secret. A request changed or replayed on the way is refused.',
  },
  {
    value: 'shared_secret',
    label: 'Secret in a header',
    detail:
      'For senders that cannot sign. The secret travels with every request, so anything that logs headers holds it, and a captured request can be sent again with a different body.',
  },
]

function ago(iso: string): string {
  const seconds = Math.round((Date.now() - new Date(iso).getTime()) / 1000)
  if (seconds < 90) return 'just now'
  const minutes = Math.round(seconds / 60)
  if (minutes < 90) return `${minutes} minutes ago`
  const hours = Math.round(minutes / 60)
  if (hours < 36) return `${hours} hours ago`
  return `${Math.round(hours / 24)} days ago`
}

type Draft = {
  id: string | null
  name: string
  prompt: string
  scheme: Scheme
  account: string
  maxPerHour: number
  enabled: boolean
}

function blank(): Draft {
  return {
    id: null,
    name: '',
    prompt: `A notification arrived. Read it and act on it:\n\n${BODY}`,
    scheme: 'hmac',
    account: '',
    maxPerHour: 60,
    enabled: true,
  }
}

function fromWebhook(w: Webhook): Draft {
  return {
    id: w.id,
    name: w.name,
    prompt: w.prompt,
    scheme: w.scheme,
    account: w.account ?? '',
    maxPerHour: w.max_per_hour,
    enabled: w.enabled,
  }
}

/** A value to hand to somebody else, with a button that copies it. */
function CopyField({ value, label: name }: { value: string; label: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <div className="flex items-center gap-1.5">
      <code className="min-w-0 flex-1 truncate px-2 py-1 rounded bg-surface-100 dark:bg-surface-900 text-xs text-surface-800 dark:text-surface-200">
        {value}
      </code>
      <button
        type="button"
        aria-label={`Copy ${name}`}
        onClick={() =>
          void navigator.clipboard.writeText(value).then(() => {
            setCopied(true)
            setTimeout(() => setCopied(false), 1500)
          })
        }
        className="p-1.5 rounded text-surface-500 hover:text-surface-800 dark:hover:text-surface-200 hover:bg-surface-100 dark:hover:bg-surface-700"
      >
        {copied ? <Check className="w-4 h-4" /> : <Copy className="w-4 h-4" />}
      </button>
    </div>
  )
}

/** How the sender proves itself, said in the terms a sender's docs use. */
function SenderGuide({ scheme }: { scheme: Scheme }) {
  return scheme === 'hmac' ? (
    <p className="text-xs text-surface-500">
      Send <code className="font-mono">x-outturn-timestamp</code> as Unix seconds, and{' '}
      <code className="font-mono">x-outturn-signature</code> as <code className="font-mono">sha256=</code> followed by the hex HMAC-SHA256 of
      the timestamp, a period and the raw body, keyed with the secret. Requests more than a few
      minutes old are refused. A request with a wrong signature is answered as though the address
      did not exist, and is not counted here.
    </p>
  ) : (
    <p className="text-xs text-surface-500">
      Send the secret as <code className="font-mono">x-outturn-token</code> on every request. A request with a wrong
      secret is answered as though the address did not exist, and is not counted here.
    </p>
  )
}

/**
 * What starts this agent from outside: an address another system POSTs to.
 *
 * Beside the schedules, because both answer what an agent does when nobody is
 * talking to it, and a hook means nothing without the agent it starts.
 */
export default function AgentWebhooks({ agentId }: { agentId: string }) {
  const state = useSession()
  const [rows, setRows] = useState<Webhook[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [draft, setDraft] = useState<Draft | null>(null)
  const [saving, setSaving] = useState(false)
  // The secret, for the one moment it can be read: after creating or
  // rotating. The API never returns it again.
  const [secret, setSecret] = useState<{ id: string; value: string } | null>(null)

  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canEdit = authorities.includes('agents:update')
  const internal = isClearlyPrivate(window.location.hostname)

  const reload = useCallback(async () => {
    try {
      setRows(await listWebhooks(agentId))
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to load webhooks')
    } finally {
      setLoading(false)
    }
  }, [agentId])

  useEffect(() => {
    void reload()
  }, [reload])

  function input(d: Draft): WebhookInput {
    return {
      agent_id: agentId,
      name: d.name,
      prompt: d.prompt,
      scheme: d.scheme,
      account: d.account.trim() || null,
      max_per_hour: d.maxPerHour,
      enabled: d.enabled,
    }
  }

  async function save() {
    if (!draft) return
    setSaving(true)
    try {
      if (draft.id) {
        await updateWebhook(draft.id, input(draft))
      } else {
        const created = await createWebhook(input(draft))
        setSecret({ id: created.id, value: created.secret })
      }
      setDraft(null)
      await reload()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to save')
    } finally {
      setSaving(false)
    }
  }

  async function toggle(w: Webhook) {
    try {
      await updateWebhook(w.id, { ...input(fromWebhook(w)), enabled: !w.enabled })
      await reload()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to save')
    }
  }

  async function rotate(w: Webhook) {
    if (
      !confirm(
        `Issue a new secret for "${w.name}"? Whoever is sending is refused until they are given it.`,
      )
    )
      return
    try {
      const { secret: value } = await rotateWebhookSecret(w.id)
      setSecret({ id: w.id, value })
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to issue a new secret')
    }
  }

  async function remove(w: Webhook) {
    if (!confirm(`Delete "${w.name}"? Its address stops working. Sessions it started are kept.`))
      return
    try {
      await deleteWebhook(w.id)
      if (secret?.id === w.id) setSecret(null)
      await reload()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to delete')
    }
  }

  if (loading) return <p className="text-sm text-surface-500">Loading…</p>

  const missingBody = draft !== null && !draft.prompt.includes(BODY)

  return (
    <section className="mt-8 space-y-4">
      <div className="flex items-baseline justify-between gap-4">
        <div>
          <h2 className="text-lg font-semibold text-surface-900 dark:text-surface-100">
            Webhooks
          </h2>
          <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
            Addresses another system posts to, each starting a turn with what it sent. Anyone with
            the address can reach it, so every request must prove it holds the secret.
          </p>
        </div>
        {canEdit && !draft && (
          <button
            type="button"
            onClick={() => setDraft(blank())}
            className="shrink-0 inline-flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-md bg-brand-600 text-white hover:bg-brand-700"
          >
            <Plus className="w-4 h-4" /> New webhook
          </button>
        )}
      </div>

      {error && (
        <p className="text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      {rows.length === 0 && !draft && (
        <p className="text-sm text-surface-500">
          None yet. A webhook might start a turn when a booking system reports a new reservation,
          or when a form is submitted.
        </p>
      )}

      <ul className="space-y-2">
        {rows.map((w) => (
          <li
            key={w.id}
            className="p-3 rounded-lg border border-surface-200 dark:border-surface-700 bg-white dark:bg-surface-800"
          >
            <div className="flex items-start justify-between gap-3">
              <button
                type="button"
                onClick={() => canEdit && setDraft(fromWebhook(w))}
                disabled={!canEdit}
                className="text-left min-w-0 flex-1 disabled:cursor-default"
              >
                <div className="flex items-center gap-2">
                  <span className="font-medium text-surface-900 dark:text-surface-100">
                    {w.name}
                  </span>
                  {!w.enabled && (
                    <span className="text-xs px-1.5 py-0.5 rounded bg-surface-200 dark:bg-surface-700 text-surface-600 dark:text-surface-400">
                      off
                    </span>
                  )}
                </div>
                <p className="text-sm text-surface-600 dark:text-surface-400">
                  {SCHEMES.find((s) => s.value === w.scheme)?.label} · up to {w.max_per_hour} an
                  hour
                </p>
              </button>

              {canEdit && (
                <div className="flex items-center gap-1 shrink-0">
                  <button
                    type="button"
                    onClick={() => void toggle(w)}
                    className="px-2 py-1 text-xs rounded border border-surface-300 dark:border-surface-600 hover:bg-surface-100 dark:hover:bg-surface-700"
                  >
                    {w.enabled ? 'Turn off' : 'Turn on'}
                  </button>
                  <button
                    type="button"
                    onClick={() => void rotate(w)}
                    aria-label={`New secret for ${w.name}`}
                    title="New secret"
                    className="p-1.5 rounded text-surface-500 hover:text-surface-800 dark:hover:text-surface-200 hover:bg-surface-100 dark:hover:bg-surface-700"
                  >
                    <KeyRound className="w-4 h-4" />
                  </button>
                  <button
                    type="button"
                    onClick={() => void remove(w)}
                    aria-label={`Delete ${w.name}`}
                    className="p-1.5 rounded text-surface-500 hover:text-red-600 hover:bg-surface-100 dark:hover:bg-surface-700"
                  >
                    <Trash2 className="w-4 h-4" />
                  </button>
                </div>
              )}
            </div>

            <div className="mt-2 space-y-1">
              <CopyField value={webhookUrl(w.endpoint)} label={`the address of ${w.name}`} />
              {internal && (
                <p className="text-xs text-surface-500">
                  This is the address as this browser reaches it. A sender outside this network
                  needs a public one.
                </p>
              )}
            </div>

            {secret?.id === w.id && (
              <div className="mt-3 p-3 rounded-md border border-amber-300 dark:border-amber-700 bg-amber-50 dark:bg-amber-950/40 space-y-2">
                <p className="text-sm text-surface-800 dark:text-surface-200">
                  The secret, shown this once. Give it to the sender now; it cannot be read again,
                  only replaced.
                </p>
                <CopyField value={secret.value} label="the secret" />
                <SenderGuide scheme={w.scheme} />
                <button
                  type="button"
                  onClick={() => setSecret(null)}
                  className="text-xs text-surface-600 dark:text-surface-400 hover:underline"
                >
                  Done
                </button>
              </div>
            )}

            <div className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-surface-500">
              {w.last_at ? (
                <span
                  className={w.last_status === 'ok' ? '' : 'text-red-600 dark:text-red-400'}
                >
                  Last request {ago(w.last_at)}
                  {w.last_status === 'failed' && ' · failed'}
                  {w.last_status === 'refused' && ' · refused'}
                </span>
              ) : (
                <span>No requests yet</span>
              )}
              {w.refused > 0 && <span>{w.refused} over the hourly limit</span>}
            </div>

            {w.last_error && w.last_status !== 'ok' && (
              <p className="mt-2 text-xs text-red-600 dark:text-red-400 flex items-start gap-1.5">
                <AlertTriangle className="w-3.5 h-3.5 mt-0.5 shrink-0" />
                {w.last_error}
              </p>
            )}
          </li>
        ))}
      </ul>

      {draft && (
        <div className="p-4 rounded-lg border border-surface-200 dark:border-surface-700 bg-surface-50 dark:bg-surface-800/50 space-y-4">
          <div>
            <label className={label} htmlFor="hook-name">
              Name
            </label>
            <input
              id="hook-name"
              className={field}
              value={draft.name}
              onChange={(e) => setDraft({ ...draft, name: e.target.value })}
              placeholder="New reservations"
            />
          </div>

          <div>
            <label className={label} htmlFor="hook-prompt">
              What should it do?
            </label>
            <textarea
              id="hook-prompt"
              className={`${field} min-h-28 font-mono text-sm`}
              value={draft.prompt}
              onChange={(e) => setDraft({ ...draft, prompt: e.target.value })}
            />
            <p
              className={`mt-1 text-xs ${missingBody ? 'text-amber-700 dark:text-amber-400' : 'text-surface-500'}`}
            >
              {missingBody ? (
                <>
                  Without <code className="font-mono">{BODY}</code> the agent never sees what was sent; every request
                  starts the same turn.
                </>
              ) : (
                <>
                  Each request becomes a new conversation starting with this, the request&rsquo;s
                  body in place of <code className="font-mono">{BODY}</code>. Whoever sends it chooses that text, so the
                  agent should be one that is safe to give anything.
                </>
              )}
            </p>
          </div>

          <div>
            <span className={label}>How the sender proves itself</span>
            <div className="space-y-2">
              {SCHEMES.map((s) => (
                <label key={s.value} className="flex items-start gap-2 cursor-pointer">
                  <input
                    type="radio"
                    name="hook-scheme"
                    className="mt-1"
                    checked={draft.scheme === s.value}
                    onChange={() => setDraft({ ...draft, scheme: s.value })}
                  />
                  <span>
                    <span className="text-sm text-surface-900 dark:text-surface-100">
                      {s.label}
                    </span>
                    <span className="block text-xs text-surface-500">{s.detail}</span>
                  </span>
                </label>
              ))}
            </div>
          </div>

          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className={label} htmlFor="hook-ceiling">
                At most, per hour
              </label>
              <input
                id="hook-ceiling"
                type="number"
                min={1}
                className={field}
                value={draft.maxPerHour}
                onChange={(e) =>
                  setDraft({ ...draft, maxPerHour: Math.max(1, Number(e.target.value) || 1) })
                }
              />
              <p className="mt-1 text-xs text-surface-500">Requests past this are refused.</p>
            </div>
            <div>
              <label className={label} htmlFor="hook-account">
                Account
              </label>
              <input
                id="hook-account"
                className={field}
                value={draft.account}
                onChange={(e) => setDraft({ ...draft, account: e.target.value })}
                placeholder="Optional"
              />
              <p className="mt-1 text-xs text-surface-500">
                Whose conversations these are, as the dashboard groups usage.
              </p>
            </div>
          </div>

          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={() => void save()}
              disabled={saving || !draft.name.trim() || !draft.prompt.trim()}
              className="px-4 py-2 text-sm rounded-md bg-brand-600 text-white hover:bg-brand-700 disabled:opacity-50"
            >
              {saving ? 'Saving…' : draft.id ? 'Save' : 'Create'}
            </button>
            <button
              type="button"
              onClick={() => setDraft(null)}
              className="px-4 py-2 text-sm rounded-md border border-surface-300 dark:border-surface-600 hover:bg-surface-100 dark:hover:bg-surface-700"
            >
              Cancel
            </button>
          </div>
        </div>
      )}
    </section>
  )
}
