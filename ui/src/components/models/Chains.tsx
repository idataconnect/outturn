import { Fragment, useState } from 'react'
import { ArrowLeft, ArrowRight, Link2, Plus, Trash2, X } from 'lucide-react'

import {
  assign,
  createChain,
  deleteChain,
  TRAFFIC_CLASSES,
  unassign,
  updateChain,
  type Account,
  type Chain,
  type Model,
} from '../../lib/models'
import { buttonPrimary, buttonQuiet, field, sectionBox } from './styles'

/** What a class of traffic is for, in a few words. */
const CLASS_LABELS: Record<string, string> = {
  assistant: 'Somebody waiting on a reply',
  background: 'Turns nobody is waiting on: schedules and webhooks',
  'session-name': 'Naming conversations',
  vision: 'Describing images',
}

function family(model: Model | undefined): string | null {
  const f = model?.facts?.family
  return typeof f === 'string' ? f : null
}

/**
 * Chains: the models a class of traffic tries, in order. A request goes to
 * the first, and to the next when one is unavailable, rate limited, or no
 * longer offered -- never when the request itself was refused, which the
 * next would refuse too. A link to a different family of model is marked,
 * since its answers will read differently.
 */
export default function Chains({
  accounts,
  models,
  chains,
  assignments,
  reload,
}: {
  accounts: Account[]
  models: Model[]
  chains: Chain[]
  assignments: Record<string, string>
  reload: () => void
}) {
  const [creating, setCreating] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const byId = new Map(models.map((m) => [m.id, m]))
  const accountName = (id: string) => accounts.find((a) => a.id === id)?.name ?? ''

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

  // Every class with a chain, and the known ones without.
  const classes = [...new Set([...TRAFFIC_CLASSES, ...Object.keys(assignments)])]

  return (
    <section className={sectionBox}>
      <div className="flex items-center justify-between gap-4">
        <div className="flex items-center gap-2">
          <Link2 size={18} className="text-surface-500" aria-hidden />
          <h2 className="text-lg font-medium text-surface-900 dark:text-surface-100">Chains</h2>
        </div>
        {!creating && models.length > 0 && (
          <button type="button" className={buttonQuiet} onClick={() => setCreating(true)}>
            <Plus size={14} aria-hidden />
            New chain
          </button>
        )}
      </div>
      <p className="text-sm text-surface-600 dark:text-surface-400">
        The models a request tries, in order. The next is asked when one is unavailable, rate limited, or no
        longer offered.
      </p>

      {chains.map((chain) => (
        <ChainEditor
          key={chain.id}
          chain={chain}
          byId={byId}
          models={models}
          accountName={accountName}
          walkedBy={classes.filter((c) => assignments[c] === chain.id)}
          attempt={attempt}
        />
      ))}

      {creating && (
        <NewChain
          models={models}
          onSave={async (name, first) => {
            if (await attempt(() => createChain(name, [first]))) setCreating(false)
          }}
          onCancel={() => setCreating(false)}
        />
      )}

      {chains.length > 0 && (
        <div className="space-y-2">
          <h3 className="text-sm font-medium text-surface-900 dark:text-surface-100">Which chain each class walks</h3>
          <table className="w-full text-sm">
            <tbody>
              {classes.map((c) => (
                <tr key={c} className="border-t border-surface-200 dark:border-surface-700">
                  <td className="py-2 pr-4">
                    <span className="font-medium text-surface-900 dark:text-surface-100">{c}</span>
                    {CLASS_LABELS[c] && <span className="block text-surface-500">{CLASS_LABELS[c]}</span>}
                  </td>
                  <td className="py-2">
                    <select
                      aria-label={`Chain for ${c}`}
                      value={assignments[c] ?? ''}
                      onChange={(e) =>
                        void attempt(() => (e.target.value ? assign(c, e.target.value) : unassign(c)))
                      }
                      className={field}
                    >
                      <option value="">
                        {c === 'background' ? 'Same as assistant' : 'None'}
                      </option>
                      {chains.map((ch) => (
                        <option key={ch.id} value={ch.id}>
                          {ch.name}
                        </option>
                      ))}
                    </select>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {error && (
        <p role="alert" className="text-sm text-red-700 dark:text-red-400">
          {error}
        </p>
      )}
    </section>
  )
}

function ChainEditor({
  chain,
  byId,
  models,
  accountName,
  walkedBy,
  attempt,
}: {
  chain: Chain
  byId: Map<string, Model>
  models: Model[]
  accountName: (id: string) => string
  walkedBy: string[]
  attempt: (what: () => Promise<unknown>) => Promise<boolean>
}) {
  const [adding, setAdding] = useState('')
  const save = (links: string[]) => attempt(() => updateChain(chain.id, chain.name, links))
  const move = (from: number, to: number) => {
    const links = [...chain.models]
    const [taken] = links.splice(from, 1)
    links.splice(to, 0, taken)
    void save(links)
  }
  const unused = models.filter((m) => !chain.models.includes(m.id))

  return (
    <div className="rounded-md border border-surface-200 dark:border-surface-700 p-3 space-y-3">
      <div className="flex items-start justify-between gap-4">
        <div>
          <p className="font-medium text-surface-900 dark:text-surface-100">{chain.name}</p>
          <p className="text-sm text-surface-500">
            {walkedBy.length > 0 ? `Walked by ${walkedBy.join(', ')}` : 'Not walked by any class'}
          </p>
        </div>
        <button
          type="button"
          className={buttonQuiet}
          aria-label={`Delete chain ${chain.name}`}
          disabled={walkedBy.length > 0}
          title={walkedBy.length > 0 ? 'A class still walks this chain' : undefined}
          onClick={() => {
            if (window.confirm(`Delete the chain ${chain.name}?`)) void attempt(() => deleteChain(chain.id))
          }}
        >
          <Trash2 size={14} aria-hidden />
        </button>
      </div>

      <ol className="flex items-stretch gap-2 overflow-x-auto pb-1" aria-label={`Models in ${chain.name}, in order`}>
        {chain.models.map((id, i) => {
          const model = byId.get(id)
          const before = i > 0 ? byId.get(chain.models[i - 1]) : undefined
          const changesFamily = i > 0 && family(model) !== family(before)
          return (
            <Fragment key={id}>
              {i > 0 && (
                <li aria-hidden className="flex flex-col items-center justify-center text-xs text-surface-500 px-1">
                  <ArrowRight size={16} />
                  if unavailable
                </li>
              )}
              <li className="w-52 shrink-0 rounded-md border border-surface-300 dark:border-surface-600 bg-surface-50 dark:bg-surface-800/50 p-2 space-y-1">
                <p className="text-sm font-medium text-surface-900 dark:text-surface-100">
                  {i + 1}. {model?.name ?? 'A deleted model'}
                </p>
                {model && (
                  <p className="text-xs text-surface-600 dark:text-surface-400 [overflow-wrap:anywhere]">
                    {accountName(model.account_id)}
                    {model.tracks ? `, tracking the latest: now ${model.model}` : `, ${model.model}`}
                  </p>
                )}
                {changesFamily && (
                  <p className="inline-block rounded bg-amber-100 dark:bg-amber-900/50 px-1.5 py-0.5 text-xs text-amber-900 dark:text-amber-200">
                    Different model family
                  </p>
                )}
                <div className="flex gap-1 pt-1">
                  <button
                    type="button"
                    className="p-1 rounded hover:bg-surface-200 dark:hover:bg-surface-700 disabled:opacity-30"
                    aria-label={`Try ${model?.name ?? 'this model'} earlier`}
                    disabled={i === 0}
                    onClick={() => move(i, i - 1)}
                  >
                    <ArrowLeft size={14} aria-hidden />
                  </button>
                  <button
                    type="button"
                    className="p-1 rounded hover:bg-surface-200 dark:hover:bg-surface-700 disabled:opacity-30"
                    aria-label={`Try ${model?.name ?? 'this model'} later`}
                    disabled={i === chain.models.length - 1}
                    onClick={() => move(i, i + 1)}
                  >
                    <ArrowRight size={14} aria-hidden />
                  </button>
                  <button
                    type="button"
                    className="p-1 rounded hover:bg-surface-200 dark:hover:bg-surface-700 disabled:opacity-30 ml-auto"
                    aria-label={`Take ${model?.name ?? 'this model'} out of ${chain.name}`}
                    disabled={chain.models.length === 1}
                    onClick={() => void save(chain.models.filter((m) => m !== id))}
                  >
                    <X size={14} aria-hidden />
                  </button>
                </div>
              </li>
            </Fragment>
          )
        })}
      </ol>

      {unused.length > 0 && (
        <div className="flex items-end gap-2">
          <label className="block text-sm text-surface-700 dark:text-surface-300 grow">
            Fall back to
            <select value={adding} onChange={(e) => setAdding(e.target.value)} className={field}>
              <option value="">Choose a model</option>
              {unused.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.name}
                </option>
              ))}
            </select>
          </label>
          <button
            type="button"
            className={buttonQuiet}
            disabled={adding === ''}
            onClick={async () => {
              if (await save([...chain.models, adding])) setAdding('')
            }}
          >
            <Plus size={14} aria-hidden />
            Add to the end
          </button>
        </div>
      )}
    </div>
  )
}

function NewChain({
  models,
  onSave,
  onCancel,
}: {
  models: Model[]
  onSave: (name: string, first: string) => Promise<void>
  onCancel: () => void
}) {
  const [name, setName] = useState('')
  const [first, setFirst] = useState(models[0]?.id ?? '')
  return (
    <form
      className="space-y-3 border-t border-surface-200 dark:border-surface-700 pt-3"
      onSubmit={(e) => {
        e.preventDefault()
        void onSave(name.trim(), first)
      }}
    >
      <label className="block text-sm text-surface-700 dark:text-surface-300">
        Name
        <input value={name} onChange={(e) => setName(e.target.value)} placeholder="High tier" className={field} required />
      </label>
      <label className="block text-sm text-surface-700 dark:text-surface-300">
        First model
        <select value={first} onChange={(e) => setFirst(e.target.value)} className={field}>
          {models.map((m) => (
            <option key={m.id} value={m.id}>
              {m.name}
            </option>
          ))}
        </select>
      </label>
      <div className="flex gap-2">
        <button type="submit" className={buttonPrimary} disabled={name.trim() === '' || first === ''}>
          Create chain
        </button>
        <button type="button" className={buttonQuiet} onClick={onCancel}>
          Cancel
        </button>
      </div>
    </form>
  )
}
