import { useState } from 'react'

import {
  acceptable,
  approvalYaml,
  basePath,
  cannotWrite,
  choiceFrom,
  type Choice,
} from '../lib/approvalSuggestion'
import type { Annotation, Risk, SkillSource } from '../lib/skills'

type Operation = NonNullable<SkillSource['outline']>['operations'][number]

const RISK_STYLE: Record<Risk, string> = {
  low: 'bg-green-100 text-green-800 dark:bg-green-900/40 dark:text-green-300',
  medium: 'bg-amber-100 text-amber-800 dark:bg-amber-900/40 dark:text-amber-300',
  high: 'bg-red-100 text-red-800 dark:bg-red-900/40 dark:text-red-300',
}

const FIELD =
  'rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 px-2 py-1'

/**
 * What the wizard guessed each operation's approval rule should be, to confirm.
 *
 * A guess from names and methods: it can read `execute` as harmless when it
 * deletes, and a risk decides what goes out without a person asked. So nothing
 * here is applied until it is confirmed into the operation's approval rule, one
 * operation at a time or all the guesses at once.
 */
export default function SuggestedApprovals({
  operations,
  annotations,
  baseUrl,
  busy,
  onConfirm,
}: {
  operations: Operation[]
  annotations: Annotation[]
  baseUrl: string | null
  busy: boolean
  /** Writes approval annotations: an operation and the YAML of its rule. */
  onConfirm: (rules: { target: string; value: string }[]) => void
}) {
  const [open, setOpen] = useState<string | null>(null)
  const [choice, setChoice] = useState<Choice | null>(null)

  const withGuess = operations.filter((o) => o.suggested_approval)
  if (withGuess.length === 0) return null
  const ruled = new Set(
    annotations.filter((a) => a.kind === 'approval' && a.level === 'operation').map((a) => a.target),
  )
  const ready = acceptable(withGuess, annotations)

  return (
    <div className="mt-4" aria-labelledby="suggested-approvals">
      <div className="flex flex-wrap items-center gap-2">
        <h3
          id="suggested-approvals"
          className="text-xs font-medium text-surface-700 dark:text-surface-300"
        >
          Suggested approvals
        </h3>
        <button
          type="button"
          disabled={busy || ready.length === 0}
          onClick={() =>
            onConfirm(
              ready.map((o) => ({
                target: o.name,
                value: approvalYaml(o, choiceFrom(o.suggested_approval!), baseUrl),
              })),
            )
          }
          className="rounded-md border border-surface-300 dark:border-surface-700 px-2 py-1 text-xs disabled:opacity-50"
        >
          Accept all suggested ({ready.length})
        </button>
      </div>
      <p className="mt-1 text-xs text-surface-600 dark:text-surface-400">
        Guessed from each operation's method and name. Nothing applies until it is confirmed, and a
        risk decides what goes out without anybody being asked.
      </p>
      <ul className="mt-2 divide-y divide-surface-200 dark:divide-surface-800 text-xs">
        {withGuess.map((o) => {
          const s = o.suggested_approval!
          const blocked = cannotWrite(s)
          const editing = open === o.name && choice
          return (
            <li key={o.name} className="py-1.5" data-testid={`suggestion-${o.name}`}>
              <div className="flex flex-wrap items-center gap-2">
                <span className="min-w-0 flex-1 truncate font-mono" title={`${o.method} ${o.path}`}>
                  {o.name}
                </span>
                <span className={`rounded px-1.5 py-0.5 ${RISK_STYLE[s.risk]}`}>{s.risk}</span>
                <span className="rounded bg-surface-100 dark:bg-surface-800 px-1.5 py-0.5">
                  {s.gate ? 'Gated' : 'Not gated'}
                </span>
                {s.amount && (
                  <span className="rounded bg-surface-100 dark:bg-surface-800 px-1.5 py-0.5 font-mono">
                    {s.amount.field}
                    {s.amount.currency ? ` · ${s.amount.currency}` : ''}
                  </span>
                )}
                {ruled.has(o.name) ? (
                  <span className="text-surface-500">Rule written</span>
                ) : (
                  <>
                    <button
                      type="button"
                      disabled={busy || !s.gate || blocked !== null}
                      aria-label={`Confirm ${o.name}`}
                      onClick={() =>
                        onConfirm([
                          {
                            target: o.name,
                            value: approvalYaml(o, choiceFrom(s), baseUrl),
                          },
                        ])
                      }
                      className="rounded-md border border-surface-300 dark:border-surface-700 px-2 py-0.5 disabled:opacity-50"
                    >
                      Confirm
                    </button>
                    <button
                      type="button"
                      aria-label={`Change ${o.name}`}
                      onClick={() => {
                        setOpen(editing ? null : o.name)
                        setChoice(editing ? null : choiceFrom(s))
                      }}
                      className="rounded-md border border-surface-300 dark:border-surface-700 px-2 py-0.5"
                    >
                      Change
                    </button>
                  </>
                )}
              </div>
              <p className="mt-0.5 text-surface-500">
                {s.reason}.{blocked && ` ${blocked}`}
              </p>
              {editing && choice && !ruled.has(o.name) && (
                <div className="mt-1.5 flex flex-wrap items-center gap-2">
                  <label className="flex items-center gap-1">
                    <input
                      type="checkbox"
                      checked={choice.gate}
                      onChange={(e) => setChoice({ ...choice, gate: e.target.checked })}
                    />
                    Needs approval
                  </label>
                  <select
                    aria-label="Risk"
                    value={choice.risk}
                    onChange={(e) => setChoice({ ...choice, risk: e.target.value as Risk })}
                    className={FIELD}
                  >
                    <option value="low">low</option>
                    <option value="medium">medium</option>
                    <option value="high">high</option>
                  </select>
                  <select
                    aria-label="Amount field"
                    value={choice.amount}
                    onChange={(e) => setChoice({ ...choice, amount: e.target.value })}
                    className={`${FIELD} font-mono`}
                  >
                    <option value="">No amount</option>
                    {[...new Set([...s.binds, ...(s.amount ? [s.amount.field] : [])])].map((f) => (
                      <option key={f} value={f}>
                        {f}
                      </option>
                    ))}
                  </select>
                  {choice.amount && (
                    <>
                      <input
                        aria-label="Currency"
                        placeholder="GBP"
                        value={choice.currency}
                        onChange={(e) => setChoice({ ...choice, currency: e.target.value })}
                        className={`${FIELD} w-16 font-mono`}
                      />
                      <input
                        aria-label="Decimal places in the smallest unit"
                        placeholder="2"
                        value={choice.minorUnits}
                        onChange={(e) => setChoice({ ...choice, minorUnits: e.target.value })}
                        className={`${FIELD} w-12 font-mono`}
                      />
                    </>
                  )}
                  <button
                    type="button"
                    disabled={busy || !choice.gate || blocked !== null}
                    onClick={() => {
                      onConfirm([{ target: o.name, value: approvalYaml(o, choice, baseUrl) }])
                      setOpen(null)
                      setChoice(null)
                    }}
                    className="rounded-md bg-brand-700 px-2 py-1 font-medium text-white disabled:opacity-50"
                  >
                    Confirm as changed
                  </button>
                </div>
              )}
            </li>
          )
        })}
      </ul>
      {basePath(baseUrl) && (
        <p className="mt-1 text-xs text-surface-500">
          Rules match paths under <span className="font-mono">{basePath(baseUrl)}</span>.
        </p>
      )}
    </div>
  )
}
