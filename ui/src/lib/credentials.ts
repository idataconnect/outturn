import { allPages, api } from './api'
import { bindingBytes, seal, toBase64 } from './seal'

/** An egress rule: a host this workspace's agents may reach, and what is sent there. */
export type EgressRule = {
  id: string
  host: string
  header: string | null
  credential_env: string | null
  credential: string | null
  enabled: boolean
  /** The header the skill that opened this host says its key travels in. */
  suggested_header?: string
}

/** A sealed credential, as anybody may see it: where it goes, never what it is. */
export type Credential = {
  id: string
  name: string
  binding: {
    kind?: string
    hosts?: string[]
    header?: string
    workspaces?: string[]
    /** For a service account: who it signs in as, and for what. */
    account?: string
    scope?: string
  }
  key_id: string
  generation: number
  created_at: string
  updated_at: string
  revoked_at: string | null
}

/** What a test proved about a key, from the same GET made with it and without.
 *  `fingerprint` names the key the gateway sent, computed by the gateway
 *  alone: the same key always shows the same one. */
export type Tested = {
  url: string
  status: number
  without_key: number | null
  verdict: 'works' | 'not_needed' | 'refused' | 'unclear'
  fingerprint: string | null
  /** The start of what the host said, when the key did not plainly work. */
  said?: string
  /** A likely cause worth checking. */
  hint?: string
}

export const listRules = () => allPages<EgressRule>('/v1/egress-rules')
export const listCredentials = () => allPages<Credential>('/v1/credentials')

/** A UUIDv7, which is what every other id here is: time-ordered, so a
 *  credential sorts where it was made. */
export function uuidv7(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16))
  let ms = Date.now()
  for (let i = 5; i >= 0; i--) {
    bytes[i] = ms & 0xff
    ms = Math.floor(ms / 256)
  }
  bytes[6] = (bytes[6] & 0x0f) | 0x70
  bytes[8] = (bytes[8] & 0x3f) | 0x80
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
}

/** A service account's key file, as Google issues it, split into what is
 *  secret and what is bound. See docs/service-accounts.md. */
export type ServiceAccountKey = { account: string; tokenUrl: string; privateKey: string }

/** Reads a pasted value as a service account key file, or null if it is not
 *  one -- in which case it is an ordinary header value. */
export function readServiceAccount(text: string): ServiceAccountKey | null {
  let file: unknown
  try {
    file = JSON.parse(text)
  } catch {
    return null
  }
  if (typeof file !== 'object' || file === null) return null
  const f = file as Record<string, unknown>
  if (
    f.type !== 'service_account' ||
    typeof f.client_email !== 'string' ||
    typeof f.private_key !== 'string' ||
    typeof f.token_uri !== 'string'
  ) {
    return null
  }
  return { account: f.client_email, tokenUrl: f.token_uri, privateKey: f.private_key }
}

/** What is being connected: a header value, or a service account's key and
 *  the scope it is exchanged for. */
export type Secret = string | { serviceAccount: ServiceAccountKey; scope: string }

/** Seals `secret` in this browser for one host and header, for `workspace`. */
async function sealFor(
  id: string,
  workspace: string,
  host: string,
  header: string,
  secret: Secret,
): Promise<{ binding: string; sealed: string; key_id: string }> {
  const key = await api<{ public_key: string; key_id: string }>('/v1/credentials/seal-key')
  const binding = bindingBytes(
    typeof secret === 'string'
      ? { credential: id, kind: 'static', workspaces: [workspace], hosts: [host], header }
      : {
          credential: id,
          kind: 'service_account',
          workspaces: [workspace],
          hosts: [host],
          header: 'authorization',
          token_url: secret.serviceAccount.tokenUrl,
          account: secret.serviceAccount.account,
          scope: secret.scope,
        },
  )
  const plaintext = typeof secret === 'string' ? secret : secret.serviceAccount.privateKey
  const sealed = await seal(key.public_key, binding, plaintext)
  return { binding: toBase64(binding), sealed: toBase64(sealed), key_id: key.key_id }
}

/**
 * Connects a key to a host the workspace already allows: sealed here, stored,
 * and put on the host's rule. The secret leaves this function only sealed.
 */
export async function connect(
  rule: EgressRule,
  workspace: string,
  name: string,
  header: string,
  secret: Secret,
): Promise<EgressRule> {
  const id = uuidv7()
  const sealed = await sealFor(id, workspace, rule.host, header, secret)
  await api<Credential>('/v1/credentials', {
    method: 'POST',
    body: JSON.stringify({ id, name, ...sealed }),
  })
  return api<EgressRule>(`/v1/egress-rules/${rule.id}`, {
    method: 'PATCH',
    body: JSON.stringify({ header, credential: id }),
  })
}

/** Replaces a connected credential's key, keeping its id and every rule naming it. */
export async function replaceKey(
  credential: string,
  workspace: string,
  host: string,
  header: string,
  secret: Secret,
): Promise<Credential> {
  const sealed = await sealFor(credential, workspace, host, header, secret)
  return api<Credential>(`/v1/credentials/${credential}`, {
    method: 'PUT',
    body: JSON.stringify(sealed),
  })
}

/** Takes the key off the rule and revokes it. The host stays allowed. */
export async function disconnect(rule: EgressRule): Promise<void> {
  await api<EgressRule>(`/v1/egress-rules/${rule.id}`, {
    method: 'PATCH',
    body: JSON.stringify({}),
  })
  if (rule.credential) {
    await api<Credential>(`/v1/credentials/${rule.credential}`, { method: 'DELETE' })
  }
}

/** Allows a host for this workspace's agents, with no key. */
export const allowHost = (host: string) =>
  api<EgressRule>('/v1/egress-rules', { method: 'POST', body: JSON.stringify({ host }) })

/** Removes a host. Its key, if any, stays stored until revoked. */
export const removeRule = (id: string) => api<void>(`/v1/egress-rules/${id}`, { method: 'DELETE' })

/** Revokes a credential no rule is using. */
export const revokeCredential = (id: string) =>
  api<Credential>(`/v1/credentials/${id}`, { method: 'DELETE' })

/** `origin` is where the host answers, `http://petstore:8080`, when that is
 *  not https on the default port; its host has to be the rule's. */
export const testCredential = (credential: string, path: string, origin?: string) =>
  api<Tested>(`/v1/credentials/${credential}/test`, {
    method: 'POST',
    body: JSON.stringify({ path, ...(origin ? { origin } : {}) }),
  })
