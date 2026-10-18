import { api } from './api'
import { uuidv7 } from './credentials'
import { bindingBytes, seal, toBase64 } from './seal'

/** The protocols an account may speak; see the gateway's providers. */
export const PROTOCOLS = ['anthropic', 'openai', 'gemini', 'bedrock'] as const
export type Protocol = (typeof PROTOCOLS)[number]

export type Account = {
  id: string
  name: string
  protocol: Protocol
  base_url: string
  credential_env: string | null
  credential_id: string | null
}

export type Adjustment = {
  workspace_id: string
  agent_id: string
  key: string
  from: unknown
  to: unknown
}

export type Model = {
  id: string
  account_id: string
  name: string
  model: string
  catalog_provider: string | null
  catalog_model: string | null
  /** The catalog entry it was configured from, or what was typed. */
  facts: Record<string, unknown> | null
  overrides: Record<string, unknown>
  /** The family it tracks, for "Latest"; null when pinned. */
  tracks: string | null
  pending_model: string | null
  pending_reason: string | null
  pending_adjustments: Adjustment[] | null
}

export type Chain = { id: string; name: string; models: string[] }

export type Configured = {
  accounts: Account[]
  models: Model[]
  chains: Chain[]
  /** Traffic class to chain id. */
  assignments: Record<string, string>
}

export type Listed = {
  id: string
  name: string | null
  family: string | null
  release_date: string | null
  status: string | null
}

export const configured = () => api<Configured>('/v1/platform/models')

export const createAccount = (input: {
  name: string
  protocol: Protocol
  base_url: string
  credential_env?: string
}) => api<Account>('/v1/platform/provider-accounts', { method: 'POST', body: JSON.stringify(input) })

export const deleteAccount = (id: string) =>
  api<void>(`/v1/platform/provider-accounts/${id}`, { method: 'DELETE' })

export const accountCatalog = (id: string) =>
  api<{ provider: string | null; models: Listed[] }>(`/v1/platform/provider-accounts/${id}/catalog`)

/**
 * Seals an account's key in this browser and stores it on the account: a
 * provider key, for every workspace, for the account's host alone. The secret
 * leaves this function only sealed. Rotating keeps the credential's id.
 */
export async function sealAccountKey(account: Account, secret: string): Promise<void> {
  const key = await api<{ public_key: string; key_id: string }>('/v1/platform/seal-key')
  const id = account.credential_id ?? uuidv7()
  const binding = bindingBytes({
    credential: id,
    kind: 'provider',
    workspaces: ['*'],
    hosts: [new URL(account.base_url).hostname.toLowerCase()],
  })
  const sealed = await seal(key.public_key, binding, secret)
  await api<void>(`/v1/platform/provider-accounts/${account.id}/key`, {
    method: 'PUT',
    body: JSON.stringify({
      id,
      name: `${account.name} key`,
      binding: toBase64(binding),
      sealed: toBase64(sealed),
      key_id: key.key_id,
    }),
  })
}

export const createModel = (input: {
  account_id: string
  model: string
  name?: string
  facts?: Record<string, unknown>
  tracks?: boolean
}) => api<Model>('/v1/platform/models', { method: 'POST', body: JSON.stringify(input) })

export const updateModel = (id: string, change: { name?: string; tracks?: boolean }) =>
  api<void>(`/v1/platform/models/${id}`, { method: 'PATCH', body: JSON.stringify(change) })

export const deleteModel = (id: string) => api<void>(`/v1/platform/models/${id}`, { method: 'DELETE' })

export const approveUpgrade = (id: string) =>
  api<{ model: string }>(`/v1/platform/models/${id}/upgrade`, { method: 'POST' })

export const createChain = (name: string, models: string[]) =>
  api<Chain>('/v1/platform/chains', { method: 'POST', body: JSON.stringify({ name, models }) })

export const updateChain = (id: string, name: string, models: string[]) =>
  api<void>(`/v1/platform/chains/${id}`, { method: 'PUT', body: JSON.stringify({ name, models }) })

export const deleteChain = (id: string) => api<void>(`/v1/platform/chains/${id}`, { method: 'DELETE' })

export const assign = (trafficType: string, chainId: string) =>
  api<void>(`/v1/platform/chain-assignments/${encodeURIComponent(trafficType)}`, {
    method: 'PUT',
    body: JSON.stringify({ chain_id: chainId }),
  })

export const unassign = (trafficType: string) =>
  api<void>(`/v1/platform/chain-assignments/${encodeURIComponent(trafficType)}`, { method: 'DELETE' })

/** The classes of traffic there are; see docs/routing.md. */
export const TRAFFIC_CLASSES = ['assistant', 'background', 'session-name', 'vision'] as const

/** What answered a test call, or why nothing did. */
export type Tested = {
  ok: boolean
  model: string | null
  endpoint: string | null
  reply: string | null
  error: string | null
  elapsed_ms: number
}

/** One short request through the chain a class walks. Spends a few tokens. */
export const testCall = (trafficType = 'assistant') =>
  api<Tested>('/v1/platform/test-call', { method: 'POST', body: JSON.stringify({ traffic_type: trafficType }) })

export const updateAccount = (id: string, change: { name?: string; base_url?: string; credential_env?: string }) =>
  api<Account>(`/v1/platform/provider-accounts/${id}`, { method: 'PATCH', body: JSON.stringify(change) })
