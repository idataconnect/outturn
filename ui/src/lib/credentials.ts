import { allPages, api } from './api'
import { bindingBytes, seal, toBase64 } from './seal'

/** Whose key a rule's requests carry: the one connected here, or each
 *  person's own. See docs/user-credentials.md. */
export type Owner = 'agent' | 'asker'

/** An egress rule: a host this workspace's agents may reach, and what is sent there. */
export type EgressRule = {
  id: string
  host: string
  /** For `asker`, `credential` is the workspace's key for turns nobody sent. */
  owner: Owner
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
    /** Whose own key this is, when it is somebody's rather than the workspace's. */
    user?: string
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

/** Takes the key off the rule and revokes it. The host stays allowed -- and
 *  an asker's rule keeps the header people's keys travel in. */
export async function disconnect(rule: EgressRule): Promise<void> {
  await api<EgressRule>(`/v1/egress-rules/${rule.id}`, {
    method: 'PATCH',
    body: JSON.stringify(rule.owner === 'asker' ? { header: rule.header } : {}),
  })
  if (rule.credential) {
    await api<Credential>(`/v1/credentials/${rule.credential}`, { method: 'DELETE' })
  }
}

/** Makes a rule's connection each person's own, or one key for everyone.
 *  Back to everyone revokes the keys people connected for it. */
export const setOwner = (rule: EgressRule, owner: Owner, header?: string) =>
  api<EgressRule>(`/v1/egress-rules/${rule.id}/owner`, {
    method: 'PUT',
    body: JSON.stringify({ owner, ...(header ? { header } : {}) }),
  })

/** A host whose connection is each person's own, and the caller's key for it. */
export type OwnConnection = {
  rule_id: string
  host: string
  header: string
  connected: { credential_id: string; connected_at: string } | null
}

export type OwnConnections = {
  connections: OwnConnection[]
  /** What to seal to: public, and handed out with the list because connecting
   *  one's own needs no authority over the workspace's keys. */
  seal_key: { public_key: string; key_id: string } | null
}

export const listOwnConnections = () => api<OwnConnections>('/v1/connections')

/** A key sealed in this browser as `user`'s own, for one host whose
 *  connection is each person's: what both a person's own route and a machine
 *  user's take. The secret leaves this function only sealed. */
async function sealOwn(
  connection: OwnConnection,
  workspace: string,
  user: string,
  key: { public_key: string; key_id: string },
  secret: string,
): Promise<string> {
  const id = uuidv7()
  const binding = bindingBytes({
    credential: id,
    kind: 'static',
    workspaces: [workspace],
    hosts: [connection.host],
    header: connection.header,
    user,
  })
  const sealed = await seal(key.public_key, binding, secret)
  return JSON.stringify({
    id,
    binding: toBase64(binding),
    sealed: toBase64(sealed),
    key_id: key.key_id,
  })
}

/** Connects the caller's own key for a host, replacing one connected before. */
export async function connectOwn(
  connection: OwnConnection,
  workspace: string,
  user: string,
  key: { public_key: string; key_id: string },
  secret: string,
): Promise<void> {
  await api<unknown>(`/v1/connections/${connection.rule_id}`, {
    method: 'PUT',
    body: await sealOwn(connection, workspace, user, key, secret),
  })
}

/** Disconnects the caller's own key for a host. */
export const disconnectOwn = (ruleId: string) =>
  api<void>(`/v1/connections/${ruleId}`, { method: 'DELETE' })

/** An account that holds keys for the turns nobody sent: nobody signs in as
 *  it, and an administrator connects its keys. See docs/user-credentials.md. */
export type MachineUser = {
  id: string
  display_name: string
  connections: OwnConnection[]
  /** The agents whose turns nobody sent carry it. */
  agents: string[]
}

export const listMachineUsers = () =>
  api<{ machine_users: MachineUser[]; seal_key: OwnConnections['seal_key'] }>('/v1/machine-users')

export const createMachineUser = (displayName: string) =>
  api<MachineUser>('/v1/machine-users', {
    method: 'POST',
    body: JSON.stringify({ display_name: displayName }),
  })

/** Deletes one and every key connected for it. */
export const deleteMachineUser = (id: string) =>
  api<void>(`/v1/machine-users/${id}`, { method: 'DELETE' })

/** Connects a machine user's key for a host, sealed as its own. */
export async function connectMachine(
  machine: string,
  connection: OwnConnection,
  workspace: string,
  key: { public_key: string; key_id: string },
  secret: string,
): Promise<void> {
  await api<unknown>(`/v1/machine-users/${machine}/connections/${connection.rule_id}`, {
    method: 'PUT',
    body: await sealOwn(connection, workspace, machine, key, secret),
  })
}

export const disconnectMachine = (machine: string, ruleId: string) =>
  api<void>(`/v1/machine-users/${machine}/connections/${ruleId}`, { method: 'DELETE' })

/** The machine user an agent's turns nobody sent carry, or null. */
export const agentMachineUser = (agent: string) =>
  api<{ machine_user_id: string | null }>(`/v1/agents/${agent}/machine-user`)

export const setAgentMachineUser = (agent: string, machineUserId: string | null) =>
  api<{ machine_user_id: string | null }>(`/v1/agents/${agent}/machine-user`, {
    method: 'PUT',
    body: JSON.stringify({ machine_user_id: machineUserId }),
  })

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
