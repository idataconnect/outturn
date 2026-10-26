import { useState } from 'react'

import {
  amountComplete,
  amountFields,
  approvalYaml,
  basePath,
  cannotWrite,
  choiceFrom,
  GROUPS,
  groupOf,
  RISK_WORDS,
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
const QUIET = 'rounded-md border border-surface-300 dark:border-surface-700 px-2 py-1 disabled:opacity-50'

/**
 * What the wizard guessed about each operation, to confirm, walked in groups:
 * money, deletes, changes, then reads.
 *
 * A guess from names and methods: it can read `execute` as harmless when it
 * deletes, and a risk decides who is asked and what a policy may let through
 * unasked. So nothing here applies until it is confirmed, one operation at a
 * time or a group's guesses at once.
 */
export default function SuggestedApprovals({
  operations,
  annotations,
  baseUrl,
  busy,
  progress = null,
  onConfirm,
}: {
  operations: Operation[]
  annotations: Annotation[]
  baseUrl: string | null
  busy: boolean
  /** Rules written so far of those asked for, while a confirm is writing. */
  progress?: { done: number; total: number } | null
  /** Writes approval annotations: an operation and the YAML of its rule. */
  onConfirm: (rules: { target: string; value: string }[]) => void
}) {
  const [open, setOpen] = useState<string | null>(null)
  const [choice, setChoice] = useState<Choice | null>(null)
  const [showReads, setShowReads] = useState(false)

  const withGuess = operations.filter((o) => o.suggested_approval)
  if (withGuess.length === 0) return null
  const ruled = new Set(
    annotations.filter((a) => a.kind === 'approval' && a.level === 'operation').map((a) => a.target),
  )
  // Left to confirm: asks first by the guess, can have a rule, has none yet.
  const left = (o: Operation) =>
    o.suggested_approval!.gate && !cannotWrite(o.suggested_approval!) && !ruled.has(o.name)
  const asking = withGuess.filter((o) => o.suggested_approval!.gate && !cannotWrite(o.suggested_approval!))
  const confirmed = asking.filter((o) => ruled.has(o.name)).length

  const row = (o: Operation) => {
    const s = o.suggested_approval!
    const blocked = cannotWrite(s)
    const editing = open === o.name && choice
    return (
      <li
        key={o.name}
        className="rounded-md border border-surface-200 dark:border-surface-800 p-3"
        data-testid={`suggestion-${o.name}`}
      >
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
          <span className="min-w-0 break-all font-mono text-sm text-surface-900 dark:text-surface-100">
            {o.name}
          </span>
          <span className={`rounded px-1.5 py-0.5 ${RISK_STYLE[s.risk]}`}>
            {RISK_WORDS[s.risk]} <span className="sr-only">risk, </span>({s.risk})
          </span>
          <span className="rounded bg-surface-100 dark:bg-surface-800 px-1.5 py-0.5">
            {s.gate ? 'Asks first' : 'Goes without asking'}
          </span>
          {s.amount && (
            <span className="rounded bg-surface-100 dark:bg-surface-800 px-1.5 py-0.5 font-mono">
              {s.amount.field}
              {s.amount.currency ? ` · ${s.amount.currency}` : ''}
            </span>
          )}
        </div>
        <p className="mt-1 break-all font-mono text-surface-500">
          {o.method.toUpperCase()} {o.path}
        </p>
        <p className="mt-0.5 text-surface-600 dark:text-surface-400">
          Guessed because {s.reason}.{blocked && ` ${blocked}`}
        </p>
        <div className="mt-2 flex flex-wrap items-center gap-2">
          {ruled.has(o.name) ? (
            <span className="text-green-700 dark:text-green-400">Confirmed</span>
          ) : (
            <>
              <button
                type="button"
                disabled={busy || !s.gate || blocked !== null}
                aria-label={`Confirm ${o.name}`}
                onClick={() =>
                  onConfirm([{ target: o.name, value: approvalYaml(o, choiceFrom(s), baseUrl) }])
                }
                className={QUIET}
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
                className={QUIET}
              >
                Change
              </button>
            </>
          )}
        </div>
        {editing && choice && !ruled.has(o.name) && (
          <div className="mt-2 space-y-2">
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={choice.gate}
                onChange={(e) => setChoice({ ...choice, gate: e.target.checked })}
              />
              Needs approval
            </label>
            <fieldset className="flex flex-wrap gap-2">
              <legend className="sr-only">Risk</legend>
              {(['low', 'medium', 'high'] as const).map((r) => (
                <label
                  key={r}
                  className={`flex items-center gap-1 rounded-md border px-2 py-1 ${
                    choice.risk === r
                      ? 'border-brand-600 dark:border-brand-400'
                      : 'border-surface-300 dark:border-surface-700'
                  }`}
                >
                  <input
                    type="radio"
                    name={`risk-${o.name}`}
                    checked={choice.risk === r}
                    onChange={() => setChoice({ ...choice, risk: r })}
                  />
                  {RISK_WORDS[r]} ({r})
                </label>
              ))}
            </fieldset>
            <div className="flex flex-wrap items-center gap-2">
              <select
                aria-label="Amount field"
                value={choice.amount}
                onChange={(e) => setChoice({ ...choice, amount: e.target.value })}
                className={`${FIELD} max-w-full font-mono`}
              >
                <option value="">No amount</option>
                {amountFields(s).map((f) => (
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
                  <input
                    aria-label="Field naming the currency"
                    placeholder="currency"
                    value={choice.currencyField}
                    onChange={(e) => setChoice({ ...choice, currencyField: e.target.value })}
                    className={`${FIELD} w-28 font-mono`}
                  />
                  {!amountComplete(choice) && (
                    <span className="w-full text-surface-500 dark:text-surface-400">
                      Without its currency and decimal places the amount is left out, and the
                      operation is asked about at the risk chosen above.
                    </span>
                  )}
                </>
              )}
            </div>
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
  }

  return (
    <div className="text-xs" aria-labelledby="suggested-approvals">
      <h2 id="suggested-approvals" className="sr-only">
        Suggested approvals
      </h2>
      <p className="text-sm text-surface-700 dark:text-surface-300" role="status">
        {progress
          ? `Writing ${progress.done} of ${progress.total}…`
          : `${confirmed} of ${asking.length} confirmed.`}
      </p>
      {GROUPS.map((g) => {
        const ops = withGuess.filter((o) => groupOf(o) === g.key)
        if (ops.length === 0) return null
        const todo = ops.filter(left)
        const reads = g.key === 'read'
        return (
          <section key={g.key} aria-labelledby={`group-${g.key}`} className="mt-6">
            <div className="flex flex-wrap items-center gap-2">
              <h3
                id={`group-${g.key}`}
                className="text-base font-medium text-surface-900 dark:text-surface-100"
              >
                {g.title} ({ops.length})
              </h3>
              {todo.length > 0 && (
                <button
                  type="button"
                  disabled={busy}
                  onClick={() =>
                    onConfirm(
                      todo.map((o) => ({
                        target: o.name,
                        value: approvalYaml(o, choiceFrom(o.suggested_approval!), baseUrl),
                      })),
                    )
                  }
                  className={`${QUIET} ml-auto`}
                >
                  Confirm the {todo.length} left as guessed
                </button>
              )}
            </div>
            <p className="mt-1 text-surface-600 dark:text-surface-400">{g.about}</p>
            {reads && !showReads ? (
              <button type="button" onClick={() => setShowReads(true)} className={`${QUIET} mt-2`}>
                Show the reads ({ops.length})
              </button>
            ) : (
              <ul className="mt-2 space-y-2">{ops.map(row)}</ul>
            )}
          </section>
        )
      })}
      {basePath(baseUrl) && (
        <p className="mt-4 text-surface-500">
          Rules match paths under <span className="font-mono">{basePath(baseUrl)}</span>.
        </p>
      )}
    </div>
  )
}
