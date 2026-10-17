import { useState } from 'react'
import { KeyRound, Plus, Server, Trash2 } from 'lucide-react'

import { createAccount, deleteAccount, PROTOCOLS, sealAccountKey, type Account, type Protocol } from '../../lib/models'
import { buttonPrimary, buttonQuiet, field, sectionBox } from './styles'

/** What each protocol is, for the form: the vendor whose wire format it is. */
const PROTOCOL_LABELS: Record<Protocol, string> = {
  anthropic: 'Anthropic',
  openai: 'OpenAI-compatible (OpenAI, ollama, llama.cpp, Groq, OpenRouter...)',
  gemini: 'Gemini',
  bedrock: 'Amazon Bedrock',
}

/** Where each protocol's own vendor is, filled in when one is chosen. */
const DEFAULT_URLS: Partial<Record<Protocol, string>> = {
  anthropic: 'https://api.anthropic.com',
  openai: 'https://api.openai.com',
  gemini: 'https://generativelanguage.googleapis.com',
}

function keyState(account: Account): string {
  if (account.credential_id) return 'Key sealed'
  if (account.credential_env) return `Key from ${account.credential_env}`
  return 'No key'
}

/**
 * Provider accounts: where requests go, and the key they are made with. A key
 * typed here is sealed in this browser to the gateway, bound to the account's
 * host, and never seen by the API.
 */
export default function Accounts({ accounts, reload }: { accounts: Account[]; reload: () => void }) {
  const [adding, setAdding] = useState(false)
  const [sealing, setSealing] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  const attempt = async (what: () => Promise<unknown>) => {
    setError(null)
    try {
      await what()
      reload()
      return true
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      return false
    }
  }

  return (
    <section className={sectionBox}>
      <div className="flex items-center justify-between gap-4">
        <div className="flex items-center gap-2">
          <Server size={18} className="text-surface-500" aria-hidden />
          <h2 className="text-lg font-medium text-surface-900 dark:text-surface-100">Accounts</h2>
        </div>
        {!adding && (
          <button type="button" className={buttonQuiet} onClick={() => setAdding(true)}>
            <Plus size={14} aria-hidden />
            Add account
          </button>
        )}
      </div>

      {accounts.length === 0 && !adding && (
        <p className="text-sm text-surface-600 dark:text-surface-400">
          No accounts yet. An account is a provider's endpoint and the key used there.
        </p>
      )}

      <ul className="divide-y divide-surface-200 dark:divide-surface-700">
        {accounts.map((account) => (
          <li key={account.id} className="py-3 space-y-2">
            <div className="flex items-start justify-between gap-4">
              <div className="min-w-0">
                <p className="font-medium text-surface-900 dark:text-surface-100">{account.name}</p>
                <p className="text-sm text-surface-600 dark:text-surface-400 [overflow-wrap:anywhere]">
                  {PROTOCOL_LABELS[account.protocol]?.split(' (')[0] ?? account.protocol} at {account.base_url}
                </p>
                <p className="text-sm text-surface-500">{keyState(account)}</p>
              </div>
              <div className="flex shrink-0 gap-2">
                <button
                  type="button"
                  className={buttonQuiet}
                  onClick={() => setSealing(sealing === account.id ? null : account.id)}
                >
                  <KeyRound size={14} aria-hidden />
                  {account.credential_id ? 'Replace key' : 'Set key'}
                </button>
                <button
                  type="button"
                  className={buttonQuiet}
                  aria-label={`Delete ${account.name}`}
                  onClick={() => {
                    if (window.confirm(`Delete ${account.name}? Its sealed key is revoked.`)) {
                      void attempt(() => deleteAccount(account.id))
                    }
                  }}
                >
                  <Trash2 size={14} aria-hidden />
                </button>
              </div>
            </div>
            {sealing === account.id && (
              <KeyForm
                account={account}
                onSeal={async (secret) => {
                  if (await attempt(() => sealAccountKey(account, secret))) setSealing(null)
                }}
                onCancel={() => setSealing(null)}
              />
            )}
          </li>
        ))}
      </ul>

      {adding && (
        <AccountForm
          onSave={async (input) => {
            if (await attempt(() => createAccount(input))) setAdding(false)
          }}
          onCancel={() => setAdding(false)}
        />
      )}

      {error && (
        <p role="alert" className="text-sm text-red-700 dark:text-red-400">
          {error}
        </p>
      )}
    </section>
  )
}

function KeyForm({
  account,
  onSeal,
  onCancel,
}: {
  account: Account
  onSeal: (secret: string) => Promise<void>
  onCancel: () => void
}) {
  const [secret, setSecret] = useState('')
  const [busy, setBusy] = useState(false)
  const host = (() => {
    try {
      return new URL(account.base_url).hostname
    } catch {
      return account.base_url
    }
  })()
  return (
    <form
      className="space-y-2"
      onSubmit={async (e) => {
        e.preventDefault()
        setBusy(true)
        await onSeal(secret)
        setSecret('')
        setBusy(false)
      }}
    >
      <label className="block text-sm text-surface-700 dark:text-surface-300">
        Key
        <input
          type="password"
          autoComplete="off"
          value={secret}
          onChange={(e) => setSecret(e.target.value)}
          className={field}
          required
        />
      </label>
      <p className="text-xs text-surface-500">
        Sealed in this browser, usable only at {host}, and only as this account's key.
        {account.credential_env && ` Replaces ${account.credential_env}.`}
      </p>
      <div className="flex gap-2">
        <button type="submit" className={buttonPrimary} disabled={busy || secret === ''}>
          Seal and save
        </button>
        <button type="button" className={buttonQuiet} onClick={onCancel}>
          Cancel
        </button>
      </div>
    </form>
  )
}

function AccountForm({
  onSave,
  onCancel,
}: {
  onSave: (input: { name: string; protocol: Protocol; base_url: string; credential_env?: string }) => Promise<void>
  onCancel: () => void
}) {
  const [name, setName] = useState('')
  const [protocol, setProtocol] = useState<Protocol>('anthropic')
  const [baseUrl, setBaseUrl] = useState(DEFAULT_URLS.anthropic ?? '')
  const [env, setEnv] = useState('')
  const [busy, setBusy] = useState(false)
  return (
    <form
      className="space-y-3 border-t border-surface-200 dark:border-surface-700 pt-3"
      onSubmit={async (e) => {
        e.preventDefault()
        setBusy(true)
        await onSave({
          name: name.trim() || baseUrl,
          protocol,
          base_url: baseUrl.trim(),
          ...(env.trim() ? { credential_env: env.trim() } : {}),
        })
        setBusy(false)
      }}
    >
      <label className="block text-sm text-surface-700 dark:text-surface-300">
        Protocol
        <select
          value={protocol}
          onChange={(e) => {
            const next = e.target.value as Protocol
            setProtocol(next)
            setBaseUrl(DEFAULT_URLS[next] ?? '')
          }}
          className={field}
        >
          {PROTOCOLS.map((p) => (
            <option key={p} value={p}>
              {PROTOCOL_LABELS[p]}
            </option>
          ))}
        </select>
      </label>
      <label className="block text-sm text-surface-700 dark:text-surface-300">
        Base URL
        <input
          type="url"
          value={baseUrl}
          onChange={(e) => setBaseUrl(e.target.value)}
          placeholder={protocol === 'bedrock' ? 'https://bedrock-runtime.us-east-1.amazonaws.com' : undefined}
          className={field}
          required
        />
      </label>
      <label className="block text-sm text-surface-700 dark:text-surface-300">
        Name
        <input value={name} onChange={(e) => setName(e.target.value)} placeholder="Anthropic" className={field} />
      </label>
      <label className="block text-sm text-surface-700 dark:text-surface-300">
        Key from the gateway's environment (optional)
        <input
          value={env}
          onChange={(e) => setEnv(e.target.value)}
          placeholder="ANTHROPIC_API_KEY"
          className={field}
        />
      </label>
      <p className="text-xs text-surface-500">
        Leave it empty for an endpoint that needs no key, or to seal one afterwards.
      </p>
      <div className="flex gap-2">
        <button type="submit" className={buttonPrimary} disabled={busy || baseUrl.trim() === ''}>
          Add account
        </button>
        <button type="button" className={buttonQuiet} onClick={onCancel}>
          Cancel
        </button>
      </div>
    </form>
  )
}
