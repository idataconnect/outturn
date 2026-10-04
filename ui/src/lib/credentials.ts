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
}

/** A sealed credential, as anybody may see it: where it goes, never what it is. */
export type Credential = {
  id: string
  name: string
  binding: { hosts?: string[]; header?: string; workspaces?: string[] }
  key_id: string
  generation: number
  created_at: string
  updated_at: string
  revoked_at: string | null
}

export type Tested = { url: string; status: number; ok: boolean }

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

/** Seals `secret` in this browser for one host and header, for `workspace`. */
async function sealFor(
  id: string,
  workspace: string,
  host: string,
  header: string,
  secret: string,
): Promise<{ binding: string; sealed: string; key_id: string }> {
  const key = await api<{ public_key: string; key_id: string }>('/v1/credentials/seal-key')
  const binding = bindingBytes({
    credential: id,
    kind: 'static',
    workspaces: [workspace],
    hosts: [host],
    header,
  })
  const sealed = await seal(key.public_key, binding, secret)
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
  secret: string,
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
  secret: string,
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

export const testCredential = (credential: string, path: string) =>
  api<Tested>(`/v1/credentials/${credential}/test`, {
    method: 'POST',
    body: JSON.stringify({ path }),
  })
