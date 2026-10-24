import { useState } from 'react'

import { ApiError } from '../lib/api'
import {
  putBands,
  revertBands,
  type BandCurrency,
  type BandOperation,
  type BandsPage,
  type BandsView,
  type WriteBands,
} from '../lib/autoApproval'
import { currencyMark, toMajor, toMinor, type Risk } from '../lib/money'
import RiskBadge from './RiskBadge'

const input =
  'block w-full rounded-md border border-surface-300 bg-white px-2 py-1.5 text-sm dark:border-surface-700 dark:bg-surface-900'
const button =
  'rounded-md border border-surface-300 px-2.5 py-1 text-xs text-surface-700 hover:bg-surface-50 disabled:opacity-50 dark:border-surface-700 dark:text-surface-300 dark:hover:bg-surface-800'
const primary =
  'rounded-md bg-brand-600 px-2.5 py-1 text-xs font-medium text-white hover:bg-brand-700 disabled:opacity-50'
const quiet = 'text-xs text-surface-600 dark:text-surface-400'

/** Bands in a line: where each level ends, and what is above them. */
function bandsLine(view: BandsView | null, base: Risk | 'its operation’s own'): string {
  const parts: string[] = []
  if (view?.low) parts.push(`low below ${view.low.display}`)
  if (view?.medium) parts.push(`medium below ${view.medium.display}`)
  if (parts.length === 0) return `Every amount is ${base} risk`
  const line = `${parts.join(', ')}, ${base} risk from there`
  return line.charAt(0).toUpperCase() + line.slice(1)
}

const sourceLabel: Record<BandOperation['source'], (op: BandOperation) => string> = {
  operation: () => 'Set for this operation',
  currency: (op) => `From the bands for every ${op.currency} operation`,
  operator: () => "The skill's defaults",
}

/**
 * Where a request that moves money falls between low, medium and high, by its
 * amount: the workspace's bands per currency and per operation, over the
 * operator's defaults, never past the operator's floor.
 */
export default function ApprovalBands({ page, onChanged }: { page: BandsPage; onChanged: () => void }) {
  return (
    <section aria-labelledby="bands-heading" className="mt-10">
      <h2 id="bands-heading" className="text-lg font-semibold text-surface-900 dark:text-surface-100">
        Bands
      </h2>
      <p className="mt-1 text-sm text-surface-600 dark:text-surface-400">
        Where a request that moves money falls by its amount. Below the low bound it is low risk, below the
        medium bound medium, and above both it has its operation's own risk. Bands are per currency and never
        converted. An operation's own bands come before its currency's, and either before the skill's
        defaults.
      </p>
      {!page.may_write && (
        <p className={`mt-2 ${quiet}`}>Changed only by people who may set policies.</p>
      )}

      {page.operations.length === 0 ? (
        <p className={`mt-4 ${quiet}`}>No agent here reaches an operation that declares an amount.</p>
      ) : (
        <>
          <h3 className="mt-5 text-sm font-semibold text-surface-900 dark:text-surface-100">By currency</h3>
          <ul className="mt-2 space-y-2">
            {page.currencies.map((c) => (
              <CurrencyBands key={c.currency} currency={c} mayWrite={page.may_write} onChanged={onChanged} />
            ))}
          </ul>

          <h3 className="mt-5 text-sm font-semibold text-surface-900 dark:text-surface-100">By operation</h3>
          <ul className="mt-2 space-y-2">
            {page.operations.map((op) => (
              <OperationBands
                key={`${op.skill_id}/${op.requires}/${op.matches}`}
                op={op}
                mayWrite={page.may_write}
                onChanged={onChanged}
              />
            ))}
          </ul>
        </>
      )}
    </section>
  )
}

function CurrencyBands({
  currency,
  mayWrite,
  onChanged,
}: {
  currency: BandCurrency
  mayWrite: boolean
  onChanged: () => void
}) {
  const [editing, setEditing] = useState(false)
  const row = currency.band
  return (
    <li className="rounded-md border border-surface-200 p-3 dark:border-surface-800">
      <p className="text-sm font-medium text-surface-900 dark:text-surface-100">
        Every {currency.currency} operation
      </p>
      <p className={`mt-0.5 ${quiet}`}>
        {row && currency.effective
          ? `${bandsLine(currency.effective, 'its operation’s own')}${
              row.updated_by_name ? ` · set by ${row.updated_by_name}` : ''
            }`
          : 'None set: each operation has its own bands, or the skill’s defaults.'}
      </p>
      <Editor
        what={`bands for every ${currency.currency} operation`}
        editing={editing}
        setEditing={setEditing}
        mayWrite={mayWrite}
        currency={currency.currency}
        minorUnits={currency.minor_units}
        initial={currency.effective}
        bandId={row?.id ?? null}
        write={(low, medium) => putBands({ currency: currency.currency, low, medium })}
        onChanged={onChanged}
      />
    </li>
  )
}

function OperationBands({ op, mayWrite, onChanged }: { op: BandOperation; mayWrite: boolean; onChanged: () => void }) {
  const [editing, setEditing] = useState(false)
  return (
    <li className="rounded-md border border-surface-200 p-3 dark:border-surface-800">
      <p className="flex flex-wrap items-center gap-2 text-sm text-surface-900 dark:text-surface-100">
        <span className="font-mono">{op.matches}</span>
        <span className="text-surface-600 dark:text-surface-400">
          {op.skill_name ? `in ${op.skill_name}, ` : ''}“{op.requires}”, by {op.field}
        </span>
        <RiskBadge risk={op.risk} />
      </p>
      <p className="mt-1 text-sm text-surface-800 dark:text-surface-200">{bandsLine(op.effective, op.risk)}</p>
      <p className={`mt-0.5 ${quiet}`}>
        {sourceLabel[op.source](op)}
        {op.source !== 'operator' && ` · the skill’s defaults: ${bandsLine(op.defaults, op.risk).replace(/^./, (c) => c.toLowerCase())}`}
      </p>
      {(op.floor.high || op.floor.medium) && (
        <p className={`mt-0.5 ${quiet}`}>
          {[
            op.floor.medium && `from ${op.floor.medium.display} at least medium`,
            op.floor.high && `from ${op.floor.high.display} high`,
          ]
            .filter(Boolean)
            .join(', ')
            .replace(/^./, (c) => c.toUpperCase())}
          , whatever the bands say: the operator's floor.
        </p>
      )}
      {op.held_at_floor && (
        <p className="mt-1 text-xs text-amber-800 dark:text-amber-300">
          The bands for every {op.currency} operation reach past this operation's floor. Here they stop at the
          floor, so a large amount is still asked about.
        </p>
      )}
      <Editor
        what="bands for this operation"
        editing={editing}
        setEditing={setEditing}
        mayWrite={mayWrite}
        currency={op.currency}
        minorUnits={op.minor_units}
        initial={op.effective}
        bandId={op.band_id}
        write={(low, medium) =>
          putBands({ skill_id: op.skill_id, requires: op.requires, matches: op.matches, low, medium } satisfies WriteBands)
        }
        onChanged={onChanged}
      />
    </li>
  )
}

/** The buttons under a set of bands, and the form they open. */
function Editor({
  what,
  editing,
  setEditing,
  mayWrite,
  currency,
  minorUnits,
  initial,
  bandId,
  write,
  onChanged,
}: {
  what: string
  editing: boolean
  setEditing: (on: boolean) => void
  mayWrite: boolean
  currency: string
  minorUnits: number
  initial: BandsView | null
  bandId: string | null
  write: (low: number | null, medium: number | null) => Promise<unknown>
  onChanged: () => void
}) {
  const [low, setLow] = useState('')
  const [medium, setMedium] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  function open() {
    setLow(initial?.low ? toMajor(initial.low.amount, minorUnits) : '')
    setMedium(initial?.medium ? toMajor(initial.medium.amount, minorUnits) : '')
    setError(null)
    setEditing(true)
  }

  async function run(what: () => Promise<unknown>) {
    setBusy(true)
    setError(null)
    try {
      await what()
      setEditing(false)
      onChanged()
    } catch (e) {
      // A band past the floor is refused with the reason, which is the thing
      // to read: it names the operation and the floor.
      setError(e instanceof ApiError ? e.message : 'that could not be saved')
    } finally {
      setBusy(false)
    }
  }

  function save(e: React.FormEvent) {
    e.preventDefault()
    const l = low.trim() ? toMinor(low, minorUnits) : null
    const m = medium.trim() ? toMinor(medium, minorUnits) : null
    if ((low.trim() && l === null) || (medium.trim() && m === null)) {
      setError(`an amount in ${currency} has at most ${minorUnits} decimal places`)
      return
    }
    if (l === null && m === null) {
      setError('bands need a low or a medium bound; reverting goes back to what stood beneath')
      return
    }
    void run(() => write(l, m))
  }

  if (!mayWrite) return null
  if (!editing) {
    return (
      <div className="mt-2 flex gap-2">
        <button type="button" className={button} onClick={open} aria-label={`Edit ${what}`}>
          Edit
        </button>
        {bandId && (
          <button
            type="button"
            className={button}
            disabled={busy}
            aria-label={`Revert ${what}`}
            onClick={() => void run(() => revertBands(bandId))}
          >
            Revert
          </button>
        )}
        {error && (
          <p role="alert" className="text-xs text-red-700 dark:text-red-400">
            {error}
          </p>
        )}
      </div>
    )
  }
  const mark = currencyMark(currency)
  return (
    <form onSubmit={save} className="mt-2 space-y-2" aria-label={`Edit ${what}`}>
      <div className="grid grid-cols-2 gap-3">
        <label className="text-xs text-surface-700 dark:text-surface-300">
          Low below ({mark})
          <input className={input} inputMode="decimal" value={low} onChange={(e) => setLow(e.target.value)} />
        </label>
        <label className="text-xs text-surface-700 dark:text-surface-300">
          Medium below ({mark})
          <input className={input} inputMode="decimal" value={medium} onChange={(e) => setMedium(e.target.value)} />
        </label>
      </div>
      <p className={quiet}>Either may be left empty. Amounts in whole {currency}.</p>
      {error && (
        <p role="alert" className="text-xs text-red-700 dark:text-red-400">
          {error}
        </p>
      )}
      <div className="flex gap-2">
        <button type="submit" className={primary} disabled={busy}>
          {busy ? 'Saving…' : 'Save'}
        </button>
        <button type="button" className={button} disabled={busy} onClick={() => setEditing(false)}>
          Cancel
        </button>
      </div>
    </form>
  )
}
