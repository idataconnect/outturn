import { allPages, api } from './api'

/**
 * A public endpoint that starts a turn when something outside the platform
 * POSTs to it. See docs/triggers.md, "Webhooks".
 */
export type Scheme = 'hmac' | 'shared_secret'

export type Webhook = {
  id: string
  agent_id: string
  name: string
  scheme: Scheme
  /** The message each delivery becomes, with `{{body}}` where the request's
   *  body goes. */
  prompt: string
  enabled: boolean
  account: string | null
  max_per_hour: number
  last_at: string | null
  last_status: 'ok' | 'failed' | 'refused' | null
  last_error: string | null
  /** Deliveries the hourly ceiling turned away, ever. Not bad signatures:
   *  those are refused before the sender proves anything, and recorded
   *  nowhere but the log, so a probe of the address creates no work. */
  refused: number
  created_at: string
  /** The path to append to wherever the deployment is reached. The API cannot
   *  know its own public host, so it says only this. */
  endpoint: string
}

export type WebhookInput = {
  agent_id: string
  name: string
  prompt: string
  scheme: Scheme
  account: string | null
  max_per_hour: number
  enabled: boolean
}

/** Where `{{body}}` goes in a prompt. */
export const BODY = '{{body}}'

export const listWebhooks = (agentId: string) =>
  allPages<Webhook>(`/v1/webhook-triggers?agent_id=${encodeURIComponent(agentId)}`)

/** The secret comes back here and never again. */
export const createWebhook = (input: WebhookInput) =>
  api<Webhook & { secret: string }>('/v1/webhook-triggers', {
    method: 'POST',
    body: JSON.stringify(input),
  })

export const updateWebhook = (id: string, input: WebhookInput) =>
  api<Webhook>(`/v1/webhook-triggers/${id}`, { method: 'PATCH', body: JSON.stringify(input) })

export const deleteWebhook = (id: string) =>
  api<void>(`/v1/webhook-triggers/${id}`, { method: 'DELETE' })

/** A new secret, which stops whoever is sending until they are given it. */
export const rotateWebhookSecret = (id: string) =>
  api<{ secret: string }>(`/v1/webhook-triggers/${id}/rotate`, { method: 'POST' })

/** The endpoint as this browser reaches the API, which is the same origin. */
export const webhookUrl = (endpoint: string, origin = window.location.origin) =>
  new URL(endpoint, origin).toString()

/**
 * Whether a host certainly cannot be reached from the public internet.
 *
 * Only the certain cases, because a false alarm teaches people to ignore the
 * line it raises: loopback and private addresses, a name with no dot in it,
 * and suffixes reserved never to resolve publicly. A private name under a
 * real domain -- `outturn.corp.example.com` -- is not caught, and is not meant
 * to be; nothing in the name says so.
 */
export function isClearlyPrivate(hostname: string): boolean {
  const host = hostname.toLowerCase().replace(/^\[|\]$/g, '').replace(/\.$/, '')
  if (host === 'localhost') return true

  // IPv6: loopback, unique local (fc00::/7) and link-local (fe80::/10).
  if (host.includes(':')) {
    return host === '::1' || /^f[cd][0-9a-f]{0,2}:/.test(host) || /^fe[89ab][0-9a-f]?:/.test(host)
  }

  const v4 = host.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/)
  if (v4) {
    const [a, b] = [Number(v4[1]), Number(v4[2])]
    return (
      a === 10 ||
      a === 127 ||
      (a === 172 && b >= 16 && b <= 31) ||
      (a === 192 && b === 168) ||
      (a === 169 && b === 254) ||
      (a === 100 && b >= 64 && b <= 127)
    )
  }

  if (!host.includes('.')) return true
  return ['.local', '.localhost', '.internal', '.test', '.home.arpa'].some((s) =>
    host.endsWith(s),
  )
}
