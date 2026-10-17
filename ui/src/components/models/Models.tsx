import { useEffect, useMemo, useState } from 'react'
import { ArrowUpCircle, Boxes, Plus, Trash2 } from 'lucide-react'

import {
  accountCatalog,
  approveUpgrade,
  createModel,
  deleteModel,
  updateModel,
  type Account,
  type Adjustment,
  type Listed,
  type Model,
} from '../../lib/models'
import { buttonPrimary, buttonQuiet, field, sectionBox } from './styles'

const PLATFORM_WORKSPACE = '00000000-0000-0000-0000-000000000001'
const NIL = '00000000-0000-0000-0000-000000000000'

/** Where an adjusted value was set, in words. */
function level(a: Adjustment): string {
  if (a.agent_id !== NIL) return 'for one agent'
  if (a.workspace_id === PLATFORM_WORKSPACE) return 'for the platform'
  return 'for one workspace'
}

function shown(value: unknown): string {
  if (value === null || value === undefined) return 'unset'
  return typeof value === 'string' ? value : JSON.stringify(value)
}

/** The facts worth a glance: what it reads at once and what it can do. */
function summary(model: Model): string {
  const facts = model.facts ?? {}
  const limit = facts.limit as { context?: number } | undefined
  const parts: string[] = []
  if (limit?.context) {
    const tokens = limit.context
    parts.push(
      tokens >= 1_000_000
        ? `${+(tokens / 1_000_000).toFixed(1)}M context`
        : `${Math.round(tokens / 1000)}k context`,
    )
  }
  if (facts.reasoning === true) parts.push('thinks')
  if (facts.tool_call === true) parts.push('uses tools')
  if (facts.temperature === false) parts.push('no temperature')
  return parts.join(' · ')
}

/**
 * The models configured at the accounts: each one either pinned to a version
 * or tracking the newest in its line. A newer version waiting on somebody is
 * shown here with why, and what approving it would change.
 */
export default function Models({
  accounts,
  models,
  reload,
}: {
  accounts: Account[]
  models: Model[]
  reload: () => void
}) {
  const [adding, setAdding] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const accountName = (id: string) => accounts.find((a) => a.id === id)?.name ?? 'an account'

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
          <Boxes size={18} className="text-surface-500" aria-hidden />
          <h2 className="text-lg font-medium text-surface-900 dark:text-surface-100">Models</h2>
        </div>
        {!adding && accounts.length > 0 && (
          <button type="button" className={buttonQuiet} onClick={() => setAdding(true)}>
            <Plus size={14} aria-hidden />
            Add model
          </button>
        )}
      </div>

      {models.length === 0 && !adding && (
        <p className="text-sm text-surface-600 dark:text-surface-400">
          {accounts.length === 0 ? 'Add an account first.' : 'No models yet.'}
        </p>
      )}

      <ul className="divide-y divide-surface-200 dark:divide-surface-700">
        {models.map((model) => (
          <li key={model.id} className="py-3 space-y-2">
            <div className="flex items-start justify-between gap-4">
              <div className="min-w-0">
                <p className="font-medium text-surface-900 dark:text-surface-100">{model.name}</p>
                <p className="text-sm text-surface-600 dark:text-surface-400 [overflow-wrap:anywhere]">
                  {model.model} at {accountName(model.account_id)}
                  {model.catalog_provider === null && ', described by hand'}
                </p>
                {summary(model) && <p className="text-sm text-surface-500">{summary(model)}</p>}
              </div>
              <div className="flex shrink-0 items-center gap-3">
                {model.catalog_provider !== null && (
                  <label className="flex items-center gap-1.5 text-sm text-surface-700 dark:text-surface-300">
                    <input
                      type="checkbox"
                      checked={model.tracks !== null}
                      onChange={(e) => void attempt(() => updateModel(model.id, { tracks: e.target.checked }))}
                    />
                    Tracks latest
                  </label>
                )}
                <button
                  type="button"
                  className={buttonQuiet}
                  aria-label={`Delete ${model.name}`}
                  onClick={() => {
                    if (window.confirm(`Delete ${model.name}?`)) void attempt(() => deleteModel(model.id))
                  }}
                >
                  <Trash2 size={14} aria-hidden />
                </button>
              </div>
            </div>
            {model.pending_model && <Pending model={model} onApprove={() => attempt(() => approveUpgrade(model.id))} />}
          </li>
        ))}
      </ul>

      {adding && (
        <ModelForm
          accounts={accounts}
          onSave={async (input) => {
            if (await attempt(() => createModel(input))) setAdding(false)
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

function Pending({ model, onApprove }: { model: Model; onApprove: () => Promise<unknown> }) {
  const [busy, setBusy] = useState(false)
  const changes = model.pending_adjustments ?? []
  return (
    <div className="rounded-md bg-amber-50 dark:bg-amber-950/40 border border-amber-200 dark:border-amber-800 p-3 space-y-2">
      <p className="text-sm text-amber-900 dark:text-amber-200">
        <span className="font-medium">{model.pending_model}</span> is available.{' '}
        {model.pending_reason && `Waiting because ${model.pending_reason}.`}
      </p>
      {changes.length > 0 && (
        <ul className="text-sm text-amber-900 dark:text-amber-200 list-disc pl-5">
          {changes.map((c) => (
            <li key={`${c.workspace_id}-${c.agent_id}-${c.key}`}>
              {c.key.replace(/_/g, ' ')} {level(c)}: {shown(c.from)} becomes {shown(c.to)}
            </li>
          ))}
        </ul>
      )}
      <button
        type="button"
        className={buttonPrimary}
        disabled={busy}
        onClick={async () => {
          setBusy(true)
          await onApprove()
          setBusy(false)
        }}
      >
        <ArrowUpCircle size={14} aria-hidden />
        {changes.length > 0 ? 'Upgrade with these changes' : 'Upgrade'}
      </button>
    </div>
  )
}

function ModelForm({
  accounts,
  onSave,
  onCancel,
}: {
  accounts: Account[]
  onSave: (input: Parameters<typeof createModel>[0]) => Promise<void>
  onCancel: () => void
}) {
  const [account, setAccount] = useState(accounts[0]?.id ?? '')
  const [listed, setListed] = useState<Listed[] | null>(null)
  const [known, setKnown] = useState(true)
  const [query, setQuery] = useState('')
  const [chosen, setChosen] = useState('')
  const [tracks, setTracks] = useState(true)
  const [busy, setBusy] = useState(false)
  // Typed facts, for a model the catalog does not have.
  const [handId, setHandId] = useState('')
  const [handName, setHandName] = useState('')
  const [context, setContext] = useState('')
  const [output, setOutput] = useState('')
  const [thinks, setThinks] = useState(true)
  const [tools, setTools] = useState(true)
  const [sampling, setSampling] = useState(true)

  useEffect(() => {
    if (!account) return
    setListed(null)
    setChosen('')
    accountCatalog(account)
      .then((c) => {
        setListed(c.models)
        setKnown(c.provider !== null)
      })
      .catch(() => {
        setListed([])
        setKnown(false)
      })
  }, [account])

  const matching = useMemo(() => {
    const q = query.trim().toLowerCase()
    return (listed ?? [])
      .filter((m) => !q || m.id.toLowerCase().includes(q) || (m.name ?? '').toLowerCase().includes(q))
      .slice(0, 50)
  }, [listed, query])

  const byHand = !known
  const save = async () => {
    setBusy(true)
    if (byHand) {
      const number = (s: string) => (s.trim() ? Number(s) : undefined)
      await onSave({
        account_id: account,
        model: handId.trim(),
        name: handName.trim() || undefined,
        facts: {
          ...(handName.trim() ? { name: handName.trim() } : {}),
          limit: { context: number(context), output: number(output) },
          reasoning: thinks,
          tool_call: tools,
          temperature: sampling,
        },
      })
    } else {
      await onSave({ account_id: account, model: chosen, tracks })
    }
    setBusy(false)
  }

  return (
    <form
      className="space-y-3 border-t border-surface-200 dark:border-surface-700 pt-3"
      onSubmit={(e) => {
        e.preventDefault()
        void save()
      }}
    >
      <label className="block text-sm text-surface-700 dark:text-surface-300">
        Account
        <select value={account} onChange={(e) => setAccount(e.target.value)} className={field}>
          {accounts.map((a) => (
            <option key={a.id} value={a.id}>
              {a.name}
            </option>
          ))}
        </select>
      </label>

      {listed === null && <p className="text-sm text-surface-500">Reading the catalog...</p>}

      {listed !== null && !byHand && (
        <>
          <label className="block text-sm text-surface-700 dark:text-surface-300">
            Model
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search the catalog"
              className={field}
            />
          </label>
          <ul role="listbox" aria-label="Catalog models" className="max-h-64 overflow-y-auto rounded-md border border-surface-200 dark:border-surface-700">
            {matching.map((m) => (
              <li key={m.id}>
                <button
                  type="button"
                  role="option"
                  aria-selected={chosen === m.id}
                  onClick={() => setChosen(m.id)}
                  className={`w-full text-left px-3 py-1.5 text-sm ${
                    chosen === m.id
                      ? 'bg-brand-50 dark:bg-brand-950 text-brand-900 dark:text-brand-100'
                      : 'hover:bg-surface-50 dark:hover:bg-surface-800 text-surface-800 dark:text-surface-200'
                  }`}
                >
                  <span className="font-medium">{m.name ?? m.id}</span>
                  <span className="text-surface-500">
                    {' '}
                    {m.id}
                    {m.release_date && `, ${m.release_date}`}
                    {m.status && `, ${m.status}`}
                  </span>
                </button>
              </li>
            ))}
            {matching.length === 0 && <li className="px-3 py-1.5 text-sm text-surface-500">Nothing matches.</li>}
          </ul>
          <label className="flex items-center gap-2 text-sm text-surface-700 dark:text-surface-300">
            <input type="checkbox" checked={tracks} onChange={(e) => setTracks(e.target.checked)} />
            Track the latest version in its line
          </label>
          <p className="text-xs text-surface-500">
            A tracked model moves to newer versions on its own, unless a value set for it is one the new version
            refuses; then it waits here for approval. An untracked model stays on this version.
          </p>
        </>
      )}

      {listed !== null && byHand && (
        <>
          <p className="text-sm text-surface-600 dark:text-surface-400">
            The catalog does not cover this account, so the model is described here.
          </p>
          <label className="block text-sm text-surface-700 dark:text-surface-300">
            Model id, as the endpoint names it
            <input value={handId} onChange={(e) => setHandId(e.target.value)} placeholder="qwen3.5" className={field} required />
          </label>
          <label className="block text-sm text-surface-700 dark:text-surface-300">
            Name
            <input value={handName} onChange={(e) => setHandName(e.target.value)} className={field} />
          </label>
          <div className="grid grid-cols-2 gap-3">
            <label className="block text-sm text-surface-700 dark:text-surface-300">
              Context window, in tokens
              <input inputMode="numeric" value={context} onChange={(e) => setContext(e.target.value)} className={field} />
            </label>
            <label className="block text-sm text-surface-700 dark:text-surface-300">
              Longest reply, in tokens
              <input inputMode="numeric" value={output} onChange={(e) => setOutput(e.target.value)} className={field} />
            </label>
          </div>
          <div className="flex flex-wrap gap-4 text-sm text-surface-700 dark:text-surface-300">
            <label className="flex items-center gap-1.5">
              <input type="checkbox" checked={thinks} onChange={(e) => setThinks(e.target.checked)} />
              Thinks before answering
            </label>
            <label className="flex items-center gap-1.5">
              <input type="checkbox" checked={tools} onChange={(e) => setTools(e.target.checked)} />
              Uses tools
            </label>
            <label className="flex items-center gap-1.5">
              <input type="checkbox" checked={sampling} onChange={(e) => setSampling(e.target.checked)} />
              Takes a temperature
            </label>
          </div>
        </>
      )}

      <div className="flex gap-2">
        <button
          type="submit"
          className={buttonPrimary}
          disabled={busy || listed === null || (byHand ? handId.trim() === '' : chosen === '')}
        >
          Add model
        </button>
        <button type="button" className={buttonQuiet} onClick={onCancel}>
          Cancel
        </button>
      </div>
    </form>
  )
}
