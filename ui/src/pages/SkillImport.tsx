import { useState } from 'react'
import { Link } from 'react-router'
import { ArrowLeft, FileUp } from 'lucide-react'

import { ApiError } from '../lib/api'
import {
  createFromOpenApi,
  previewOpenApi,
  type OpenApiPreview,
  type Skill,
} from '../lib/skills'
import { useSession } from '../lib/session'

const input =
  'mt-1 w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 text-sm text-surface-900 dark:text-surface-100'
const hint = 'mt-1 block text-xs text-surface-600 dark:text-surface-400'
const label = 'text-sm font-medium text-surface-800 dark:text-surface-200'
const primary =
  'px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium disabled:opacity-50'

type Form = {
  name: string
  slug: string
  description: string
  base_url: string
  auth_header: string
  credential_env: string
}

/**
 * Turns an OpenAPI specification into an operator skill, in three steps: take
 * the document, prefill what it says about itself, and create -- then say what
 * a browser cannot do. See `docs/openapi-wizard.md`, "The page".
 *
 * Upload is the path that must always work, because it also answers every
 * specification behind a login: the person fetches it with their own
 * credentials and none of them pass through the platform.
 */
export default function SkillImport() {
  const state = useSession()
  const isOperator =
    state.status === 'authenticated' && state.session.roles.includes('system_admin')

  const [text, setText] = useState('')
  const [url, setUrl] = useState('')
  const [spec, setSpec] = useState<unknown>(null)
  const [preview, setPreview] = useState<OpenApiPreview | null>(null)
  const [form, setForm] = useState<Form | null>(null)
  const [created, setCreated] = useState<{ skill: Skill; form: Form } | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const read = async (raw: string) => {
    setError(null)
    let parsed: unknown
    try {
      parsed = JSON.parse(raw)
    } catch {
      setError(
        'That is not JSON. Only JSON specifications are read; a YAML one needs converting first.',
      )
      return
    }
    await previewFrom({ spec: parsed })
  }

  const previewFrom = async (source: { spec: unknown } | { url: string }) => {
    setError(null)
    setBusy(true)
    try {
      const p = await previewOpenApi(source)
      setSpec('spec' in source ? source.spec : p.spec)
      setPreview(p)
      setForm({
        name: p.name,
        slug: p.slug,
        description: p.description,
        base_url: p.base_url ?? '',
        auth_header: p.auth_header ?? '',
        credential_env: p.credential_env,
      })
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'could not read the specification')
    } finally {
      setBusy(false)
    }
  }

  const upload = async (file: File | undefined) => {
    if (!file) return
    const raw = await file.text()
    setText(raw.length > 200_000 ? '' : raw)
    await read(raw)
  }

  const create = async () => {
    if (!form) return
    setBusy(true)
    setError(null)
    try {
      const skill = await createFromOpenApi({
        slug: form.slug.trim(),
        name: form.name.trim(),
        description: form.description.trim(),
        base_url: form.base_url.trim().replace(/\/+$/, ''),
        auth_header: form.auth_header.trim(),
        spec,
      })
      setCreated({ skill, form })
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'could not create the skill')
    } finally {
      setBusy(false)
    }
  }

  const set = (key: keyof Form) => (e: React.ChangeEvent<HTMLInputElement | HTMLTextAreaElement>) =>
    setForm((f) => (f ? { ...f, [key]: e.target.value } : f))

  return (
    <div className="p-6 max-w-3xl">
      <Link
        to="/skills"
        className="inline-flex items-center gap-1 text-sm text-surface-600 dark:text-surface-400 hover:underline underline-offset-2"
      >
        <ArrowLeft size={14} aria-hidden />
        Skills
      </Link>
      <h1 className="mt-2 text-2xl font-semibold text-surface-900 dark:text-surface-100">
        Import from OpenAPI
      </h1>
      <p className="mt-2 text-surface-600 dark:text-surface-400">
        A specification becomes a skill every workspace can bind: a short manifest the agent
        carries on every turn, and a file per operation it reads when it needs one.
      </p>

      {!isOperator && (
        <p className="mt-4 text-sm text-surface-600 dark:text-surface-400">
          Only the operator imports specifications.
        </p>
      )}

      {error && (
        <p className="mt-4 text-sm text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}

      {isOperator && !preview && (
        <div className="mt-6 space-y-4">
          <label className="flex items-center gap-2 w-fit cursor-pointer px-4 py-2 rounded-md border border-surface-300 dark:border-surface-700 text-sm text-surface-800 dark:text-surface-200 hover:bg-surface-100 dark:hover:bg-surface-800">
            <FileUp size={16} aria-hidden />
            Upload a specification
            <input
              type="file"
              accept=".json,application/json"
              className="sr-only"
              disabled={busy}
              onChange={(e) => void upload(e.target.files?.[0])}
            />
          </label>
          <form
            className="block"
            onSubmit={(e) => {
              e.preventDefault()
              void previewFrom({ url: url.trim() })
            }}
          >
            <label className="block">
              <span className={label}>Or fetch it from a URL</span>
              <div className="flex gap-2">
                <input
                  type="url"
                  value={url}
                  onChange={(e) => setUrl(e.target.value)}
                  placeholder="https://api.example.com/openapi.json"
                  className={`${input} font-mono`}
                />
                <button type="submit" className={`${primary} mt-1 shrink-0`} disabled={busy || !url.trim()}>
                  {busy ? 'Fetching…' : 'Fetch'}
                </button>
              </div>
              <span className={hint}>
                Fetched by the gateway, without credentials. A specification behind a login is
                downloaded and uploaded instead.
              </span>
            </label>
          </form>
          <label className="block">
            <span className={label}>Or paste it</span>
            <textarea
              value={text}
              onChange={(e) => setText(e.target.value)}
              rows={12}
              placeholder='{"openapi": "3.0.0", ...}'
              className={`${input} font-mono`}
            />
          </label>
          <button
            type="button"
            className={primary}
            disabled={busy || !text.trim()}
            onClick={() => void read(text)}
          >
            {busy ? 'Reading…' : 'Read specification'}
          </button>
        </div>
      )}

      {preview && form && !created && (
        <form
          className="mt-6 space-y-4"
          onSubmit={(e) => {
            e.preventDefault()
            void create()
          }}
        >
          <p className="text-sm text-surface-700 dark:text-surface-300">
            {preview.operations} operations
            {preview.categories > 1 && ` in ${preview.categories} categories`}. Everything below
            is what the specification said about itself, and all of it can be changed.
          </p>

          <label className="block">
            <span className={label}>Name</span>
            <input value={form.name} onChange={set('name')} className={input} />
          </label>
          <label className="block">
            <span className={label}>Slug</span>
            <input value={form.slug} onChange={set('slug')} className={`${input} font-mono`} />
            <span className={hint}>
              Lowercase letters, digits and hyphens. Agents read its files as{' '}
              <code>skill/{form.slug || '…'}/…</code>
            </span>
          </label>
          <label className="block">
            <span className={label}>Description</span>
            <textarea value={form.description} onChange={set('description')} rows={2} className={input} />
          </label>
          <label className="block">
            <span className={label}>Base URL</span>
            <input
              value={form.base_url}
              onChange={set('base_url')}
              placeholder="https://api.example.com"
              className={`${input} font-mono`}
            />
            {!preview.base_url && (
              <span className="mt-1 block text-xs text-amber-700 dark:text-amber-500">
                The specification names no server, so this has to come from you.
              </span>
            )}
          </label>
          <label className="block">
            <span className={label}>Credential header</span>
            <input
              value={form.auth_header}
              onChange={set('auth_header')}
              placeholder="Authorization"
              className={`${input} font-mono`}
            />
            <span className={hint}>
              {preview.auth_header
                ? 'Named by the specification.'
                : 'The specification does not say; leave empty if the API takes no credential.'}
            </span>
          </label>
          <label className="block">
            <span className={label}>Credential variable</span>
            <input
              value={form.credential_env}
              onChange={set('credential_env')}
              className={`${input} font-mono`}
            />
            <span className={hint}>
              The environment variable on the gateway that holds the credential. Only its name
              is stored.
            </span>
          </label>

          <div className="flex items-center gap-2">
            <button
              type="submit"
              className={primary}
              disabled={busy || !form.name.trim() || !form.slug.trim() || !form.base_url.trim()}
            >
              {busy ? 'Creating…' : 'Create skill'}
            </button>
            <button
              type="button"
              className="px-4 py-2 rounded-md text-sm text-surface-700 dark:text-surface-300 hover:bg-surface-100 dark:hover:bg-surface-800"
              onClick={() => {
                setPreview(null)
                setForm(null)
                setSpec(null)
              }}
            >
              Use a different specification
            </button>
          </div>
        </form>
      )}

      {created && <StillNeeded skill={created.skill} form={created.form} />}
    </div>
  )
}

/** What the browser cannot do. The skill exists, but nothing it names is
 *  reachable until somebody does these. */
function StillNeeded({ skill, form }: { skill: Skill; form: Form }) {
  const header = form.auth_header.trim()
  const variable = form.credential_env.trim()
  const rule = JSON.stringify(
    header && variable
      ? { host: skill.hosts[0], header, credential_env: variable }
      : { host: skill.hosts[0] },
    null,
    2,
  )
  return (
    <div className="mt-6 space-y-4 text-sm text-surface-800 dark:text-surface-200">
      <p>
        Created{' '}
        <Link to={`/skills/${skill.id}`} className="underline underline-offset-2">
          {skill.name}
        </Link>
        . It is not usable yet. Two things are still needed, and neither can be done from
        here:
      </p>
      <ol className="list-decimal pl-5 space-y-3">
        {header && variable && (
          <li>
            <strong>Set <code>{variable}</code> on the gateway</strong> to the value the{' '}
            <code>{header}</code> header should carry, exactly as sent
            {header.toLowerCase() === 'authorization' && (
              <> — including the scheme, such as <code>Bearer </code>, if the API wants one</>
            )}.
            The gateway reads it from its own environment; a browser cannot set it.
          </li>
        )}
        <li>
          <strong>Allow <code>{skill.hosts.join(', ')}</code></strong> in each workspace that
          binds this skill, with an egress rule through <code>POST /v1/egress-rules</code> —
          there is no page for it yet:
          <pre className="mt-2 p-3 rounded-md bg-surface-100 dark:bg-surface-800 font-mono text-xs overflow-x-auto">
            {rule}
          </pre>
        </li>
      </ol>
    </div>
  )
}
