import { useCallback, useEffect, useState } from 'react'

import { ApiError, api } from '../lib/api'

type Kind =
  | { type: 'number'; min: number; max: number; step: number; nullable: boolean }
  | { type: 'integer'; min: number; max: number }
  | { type: 'choice'; options: { value: string; label: string }[] }

export type Effective = {
  key: string
  label: string
  description: string
  kind: Kind
  default: unknown
  owner: 'operator_only' | 'workspace_overridable' | 'operator_ceiling'
  value: unknown
  source: 'default' | 'operator' | 'workspace' | 'agent' | 'template'
  override_value: unknown | undefined
  inherited: unknown
  /** About a model, and so may also be chosen for one model. */
  per_model: boolean
}

/** A model this level may choose values for, from `/v1/models`. */
type Offered = { id: string; name: string; model: string; facts: Record<string, unknown> | null }

/** The effort levels a model's catalog entry offers, if it lists any. */
function effortsOf(model: Offered | undefined): string[] | null {
  const options = model?.facts?.reasoning_options
  if (!Array.isArray(options)) return null
  const effort = options.find((o) => o?.type === 'effort')
  return Array.isArray(effort?.values) ? (effort.values as string[]) : null
}

/** Narrows a setting's choices to those a model takes: an effort level it
 *  does not offer is not offered. Off is always offered; a model that cannot
 *  stop thinking is asked for its least instead. */
function forModel(setting: Effective, model: Offered | undefined): Effective {
  if (setting.key !== 'reasoning_effort' || setting.kind.type !== 'choice') return setting
  const efforts = effortsOf(model)
  if (!efforts) return setting
  const options = setting.kind.options.filter(
    (o) => o.value.startsWith('none') || efforts.includes(o.value),
  )
  return { ...setting, kind: { ...setting.kind, options } }
}

const SOURCE_LABEL: Record<Effective['source'], string> = {
  default: 'the built-in default',
  operator: 'the platform',
  workspace: 'this workspace',
  agent: 'this agent',
  template: 'the template from which this agent is derived',
}

function show(value: unknown): string {
  if (value === null || value === undefined) return 'Provider default'
  return String(value)
}

/**
 * One level's view of the settings catalog.
 *
 * Each setting shows the value that applies and where it came from. Beside it
 * an Override checkbox: off, the value is inherited and shown grayed; on, a
 * row exists at this level and the control is live. Turning it off deletes
 * the row and the level falls back to whatever is above it. Nothing is copied
 * down, so an operator changing a default reaches every level that never
 * chose otherwise.
 *
 * `base` is where this level's settings live: `/v1/settings` for the workspace,
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
  // The model whose values are shown, or '' for the values every model gets.
  const [model, setModel] = useState('')
  const [models, setModels] = useState<Offered[]>([])
  const query = model ? `?model=${model}` : ''

  // Who cannot read the models simply sees no picker.
  useEffect(() => {
    api<Offered[]>('/v1/models')
      .then(setModels)
      .catch(() => setModels([]))
  }, [])

  const load = useCallback(async () => {
    try {
      setSettings(await api<Effective[]>(`${base}${query}`))
      setError(null)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'failed to load settings')
    }
  }, [base, query])

  useEffect(() => {
    void load()
  }, [load])

  async function write(key: string, value: unknown) {
    setBusy(key)
    try {
      await api<void>(`${base}/${key}${query}`, { method: 'PUT', body: JSON.stringify({ value }) })
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
      await api<void>(`${base}/${key}${query}`, { method: 'DELETE' })
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

  const chosen = models.find((m) => m.id === model)
  const shownSettings = model
    ? settings?.filter((s) => s.per_model).map((s) => forModel(s, chosen))
    : settings
  const scope = chosen ? `${levelName}, for ${chosen.name}` : levelName

  return (
    <div className="space-y-4">
      {models.length > 0 && (
        <div className="space-y-1">
          <label className="flex items-center gap-3 text-sm text-surface-700 dark:text-surface-300">
            Values for
            <select
              value={model}
              onChange={(e) => {
                setError(null)
                setModel(e.target.value)
              }}
              className={field}
            >
              <option value="">Every model</option>
              {models.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.name}
                </option>
              ))}
            </select>
          </label>
          <p className="text-xs text-surface-500 dark:text-surface-400">
            {chosen
              ? `A value set here applies to ${chosen.name} alone and wins over one set for every model at the same level. Only what the model accepts can be chosen.`
              : 'Temperature, thinking and conversation size can also be set for one model. A value for every model is skipped for a model that cannot take it.'}
          </p>
        </div>
      )}
      {error && (
        <p className="text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}
      {shownSettings?.map((s) => {
        const overridden = s.override_value !== undefined
        // A value a template fixes is not this level's to change, whoever
        // is looking.
        // A ceiling the operator has set to its strictest leaves a workspace
        // nothing to choose: it may only be stricter, and there is nothing
        // stricter.
        const capped =
          s.owner === 'operator_ceiling' &&
          s.kind.type === 'choice' &&
          s.inherited === s.kind.options[0]?.value
        const locked =
          !canEdit || s.owner === 'operator_only' || s.source === 'template' || capped
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
                  s.source === 'template'
                    ? 'Defined by the template from which this agent is derived'
                    : capped
                      ? 'Turned off by the platform operator'
                      : s.owner === 'operator_only'
                      ? 'Set by the platform operator and not overridable here'
                      : `Override for ${scope}`
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
                  ? `Set for ${scope}. Without it: ${show(s.inherited)}, from ${
                      s.source === 'default' ? SOURCE_LABEL.default : 'above'
                    }.`
                  : s.source === 'template'
                    ? 'This value is defined by the template from which this agent is derived.'
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
  'px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100 disabled:opacity-60'

export function Control({
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
          <option key={o.value} value={o.value}>
            {o.label}
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
