import { useCallback, useEffect, useState } from 'react'

import { ApiError, api } from '../lib/api'

type Kind =
  | { type: 'number'; min: number; max: number; step: number; nullable: boolean }
  | { type: 'integer'; min: number; max: number }
  | { type: 'choice'; options: string[] }

export type Effective = {
  key: string
  label: string
  description: string
  kind: Kind
  default: unknown
  owner: 'operator_only' | 'tenant_overridable'
  value: unknown
  source: 'default' | 'operator' | 'tenant' | 'agent'
  override_value: unknown | undefined
  inherited: unknown
}

const SOURCE_LABEL: Record<Effective['source'], string> = {
  default: 'the built-in default',
  operator: 'the platform',
  tenant: 'this workspace',
  agent: 'this agent',
}

function show(value: unknown): string {
  if (value === null || value === undefined) return 'Provider default'
  return String(value)
}

/**
 * One level's view of the settings catalogue.
 *
 * Each setting shows the value that applies and where it came from. Beside it
 * an Override checkbox: off, the value is inherited and shown greyed; on, a
 * row exists at this level and the control is live. Turning it off deletes
 * the row and the level falls back to whatever is above it. Nothing is copied
 * down, so an operator changing a default reaches every level that never
 * chose otherwise.
 *
 * `base` is where this level's settings live: `/v1/settings` for the tenant,
 * `/v1/platform/settings` for the operator, `/v1/agents/:id/settings` for an
 * agent. The component does not know which it is drawing; the API's answer
 * says what applies here and what this level may change.
 */
export default function SettingsCascade({
  base,
  canEdit,
  levelName,
}: {
  base: string
  canEdit: boolean
  /** "this workspace", "this agent", "the platform": for the toggle's label. */
  levelName: string
}) {
  const [settings, setSettings] = useState<Effective[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)

  const load = useCallback(async () => {
    try {
      setSettings(await api<Effective[]>(base))
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to load settings')
    }
  }, [base])

  useEffect(() => {
    void load()
  }, [load])

  async function write(key: string, value: unknown) {
    setBusy(key)
    try {
      await api<void>(`${base}/${key}`, { method: 'PUT', body: JSON.stringify({ value }) })
      await load()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to save setting')
    } finally {
      setBusy(null)
    }
  }

  async function clear(key: string) {
    setBusy(key)
    try {
      await api<void>(`${base}/${key}`, { method: 'DELETE' })
      await load()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to clear setting')
    } finally {
      setBusy(null)
    }
  }

  if (settings === null && !error) {
    return <p className="text-sm text-surface-600 dark:text-surface-400">Loading…</p>
  }

  return (
    <div className="space-y-4">
      {error && (
        <p className="text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}
      {settings?.map((s) => {
        const overridden = s.override_value !== undefined
        const locked = !canEdit || s.owner === 'operator_only'
        return (
          <div
            key={s.key}
            className="rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 p-4"
          >
            <div className="flex flex-wrap items-start justify-between gap-3">
              <div className="min-w-0 flex-1">
                <p className="text-sm font-medium text-surface-900 dark:text-surface-100">
                  {s.label}
                </p>
                <p className="mt-1 text-xs text-surface-600 dark:text-surface-400">
                  {s.description}
                </p>
              </div>
              <label
                className={`flex items-center gap-2 text-sm ${
                  locked
                    ? 'text-surface-400 dark:text-surface-600'
                    : 'text-surface-700 dark:text-surface-300'
                }`}
                title={
                  s.owner === 'operator_only'
                    ? 'Set by the platform operator and not overridable here'
                    : `Override for ${levelName}`
                }
              >
                <input
                  type="checkbox"
                  checked={overridden}
                  disabled={locked || busy === s.key}
                  onChange={(e) => {
                    // Turning on starts from what applies now, so nothing
                    // changes until the value is edited. Turning off drops
                    // the row and falls back.
                    if (e.target.checked) void write(s.key, s.value)
                    else void clear(s.key)
                  }}
                />
                Override
              </label>
            </div>

            <div className="mt-3 flex flex-wrap items-center gap-3">
              <Control
                setting={s}
                disabled={!overridden || locked || busy === s.key}
                onChange={(v) => void write(s.key, v)}
              />
              <span className="text-xs text-surface-500 dark:text-surface-400">
                {overridden
                  ? `Set for ${levelName}. Without it: ${show(s.inherited)}, from ${
                      s.source === 'default' ? SOURCE_LABEL.default : 'above'
                    }.`
                  : `From ${SOURCE_LABEL[s.source]}.`}
              </span>
            </div>
          </div>
        )
      })}
    </div>
  )
}

const field =
  'px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100 disabled:opacity-60'

function Control({
  setting,
  disabled,
  onChange,
}: {
  setting: Effective
  disabled: boolean
  onChange: (value: unknown) => void
}) {
  const k = setting.kind
  const v = setting.value

  if (k.type === 'choice') {
    return (
      <select
        value={typeof v === 'string' ? v : ''}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
        className={field}
        aria-label={setting.label}
      >
        {k.options.map((o) => (
          <option key={o} value={o}>
            {o}
          </option>
        ))}
      </select>
    )
  }

  if (k.type === 'integer') {
    return (
      <input
        type="number"
        min={k.min}
        max={k.max}
        step={1}
        value={typeof v === 'number' ? v : ''}
        disabled={disabled}
        onChange={(e) => {
          const n = Number(e.target.value)
          if (Number.isInteger(n)) onChange(n)
        }}
        className={`${field} w-32`}
        aria-label={setting.label}
      />
    )
  }

  // number, possibly nullable
  const unset = v === null || v === undefined
  return (
    <span className="flex items-center gap-3">
      {k.nullable && (
        <label className="flex items-center gap-1 text-xs text-surface-600 dark:text-surface-400">
          <input
            type="checkbox"
            checked={unset}
            disabled={disabled}
            onChange={(e) => onChange(e.target.checked ? null : k.min + (k.max - k.min) / 2)}
          />
          Provider default
        </label>
      )}
      <input
        type="number"
        min={k.min}
        max={k.max}
        step={k.step}
        value={typeof v === 'number' ? v : ''}
        disabled={disabled || unset}
        onChange={(e) => {
          const n = Number(e.target.value)
          if (!Number.isNaN(n)) onChange(n)
        }}
        className={`${field} w-28`}
        aria-label={setting.label}
      />
    </span>
  )
}
