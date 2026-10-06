import { useState } from 'react'
import { Link } from 'react-router'
import { ArrowLeft, FileUp } from 'lucide-react'

import { ApiError } from '../lib/api'
import {
  createFromOpenApi,
  previewOpenApi,
  authKindLabel,
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

  // Sent as text: the API tells JSON from YAML, so there is one reader of
  // the format rather than two that might disagree.
  const read = (raw: string) => previewFrom({ spec: raw })

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
              accept=".json,.yaml,.yml,application/json,application/yaml,text/yaml"
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
                Fetched by the gateway, without credentials. A specification behind a login must
                be fetched manually and uploaded using the button above.
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
              {preview.auth_kind
                ? `Named by the specification, for ${authKindLabel[preview.auth_kind]}.`
                : preview.auth_header
                  ? 'Guessed from a header every operation takes.'
                  : preview.unserved_operations > 0
                    ? 'None of the schemes the specification declares travels in a header a rule can carry.'
                    : 'The specification does not say; leave empty if the API takes no credential.'}
              {preview.auth_kind === 'basic' &&
                ' The key connected for it is the whole value: Basic and the encoded credentials.'}
            </span>
            {preview.unserved_operations > 0 && (
              <span className="mt-1 block text-xs text-amber-700 dark:text-amber-500">
                {preview.unserved_operations} of {preview.operations} operations need{' '}
                {list(preview.unserved_kinds.map((k) => authKindLabel[k]))}
                {preview.auth_header ? ' rather than this header' : ''}. An egress rule attaches
                one static header, so the skill will not authenticate{' '}
                {preview.unserved_operations === preview.operations ? '' : 'those '}on its own.
              </span>
            )}
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

      {created && preview && (
        <StillNeeded skill={created.skill} form={created.form} preview={preview} />
      )}
    </div>
  )
}

/**
 * What happens next. The skill exists; it reaches nothing until a workspace
 * allows its host and connects a key, and both are done on the skill's own
 * page, in the workspace that will use it -- each workspace its own key,
 * sealed in its own browser. Nothing is left for the gateway's environment.
 */
function StillNeeded({ skill, form, preview }: { skill: Skill; form: Form; preview: OpenApiPreview }) {
  const header = form.auth_header.trim()
  return (
    <div className="mt-6 space-y-4 text-sm text-surface-800 dark:text-surface-200">
      <p>
        Created{' '}
        <Link to={`/skills/${skill.id}`} className="underline underline-offset-2">
          {skill.name}
        </Link>
        . In each workspace that will use it, open it and:
      </p>
      <ol className="list-decimal pl-5 space-y-3">
        <li>
          <strong>Allow {skill.hosts.join(', ')}</strong>, with the button the page shows
          while the host is not yet allowed.
        </li>
        {header && (
          <li>
            <strong>Connect a key</strong> under Connections: the whole value the{' '}
            <code>{header}</code> header should carry
            {header.toLowerCase() === 'authorization' && (
              <>, including the scheme, such as <code>Bearer </code>, if the API wants one</>
            )}
            . It is sealed in that browser, and a test call says straight away whether it works.
          </li>
        )}
        <li>
          <strong>Give it to an agent</strong> on the agent&apos;s page.
        </li>
      </ol>
      <p>
        <Link
          to={`/skills/${skill.id}`}
          className="inline-flex items-center gap-1 rounded-md bg-brand-700 px-3 py-1.5 text-white hover:bg-brand-600"
        >
          Open {skill.name}
        </Link>
      </p>
      {preview.unserved_operations > 0 && (
        <p className="text-amber-700 dark:text-amber-500">
          {preview.unserved_operations} of {preview.operations} operations need{' '}
          {list(preview.unserved_kinds.map((k) => authKindLabel[k]))}, which a connected key
          cannot supply. The skill will not authenticate{' '}
          {preview.unserved_operations === preview.operations ? '' : 'those '}on its own.
        </p>
      )}
    </div>
  )
}

/** `a`, `a or b`, `a, b or c`. */
function list(items: string[]): string {
  return items.length < 2
    ? (items[0] ?? '')
    : `${items.slice(0, -1).join(', ')} or ${items[items.length - 1]}`
}
