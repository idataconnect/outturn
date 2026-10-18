import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { CheckCircle2, CircleAlert, Loader2, Sparkles } from 'lucide-react'

import { heldCatalog, type HeldCatalog } from '../lib/catalog'
import {
  assign,
  configured,
  createAccount,
  createChain,
  createModel,
  sealAccountKey,
  testCall,
  type Account,
  type Configured,
  type Protocol,
  type Tested,
} from '../lib/models'
import { paths } from '../lib/paths'
import { buttonPrimary, buttonQuiet, field, sectionBox } from '../components/models/styles'

/** A provider a recipe needs an account at. */
type Vendor = {
  key: 'anthropic' | 'gemini'
  name: string
  protocol: Protocol
  baseUrl: string
  /** Where its key is usually found, offered for an operator whose keys
   *  live in the gateway's environment. */
  env: string
}

const VENDORS: Record<Vendor['key'], Vendor> = {
  anthropic: {
    key: 'anthropic',
    name: 'Anthropic',
    protocol: 'anthropic',
    baseUrl: 'https://api.anthropic.com',
    env: 'ANTHROPIC_API_KEY',
  },
  gemini: {
    key: 'gemini',
    name: 'Google Gemini',
    protocol: 'gemini',
    baseUrl: 'https://generativelanguage.googleapis.com',
    env: 'GEMINI_API_KEY',
  },
}

type Link_ = { vendor: Vendor['key']; model: string; label: string }

type Recipe = {
  id: string
  title: string
  summary: string
  /** In order: the first is asked, the next when it is unavailable. */
  links: Link_[]
  /** What changes when the chain falls over, said before it is chosen. */
  caveat: string
}

/** Pairs of the same tier from two vendors, so a fallback answers about as
 *  well as what it stands in for; the tier is the choice. Each tracks the
 *  latest in its line. */
const RECIPES: Recipe[] = [
  {
    id: 'high',
    title: 'High tier',
    summary: 'Claude Opus, falling back to Gemini Pro.',
    links: [
      { vendor: 'anthropic', model: 'claude-opus-5-5', label: 'Claude Opus (latest)' },
      { vendor: 'gemini', model: 'gemini-2.5-pro', label: 'Gemini Pro (latest)' },
    ],
    caveat: 'A different family of model answers on fallback.',
  },
  {
    id: 'medium',
    title: 'Medium tier',
    summary: 'Claude Sonnet, falling back to Gemini Flash.',
    links: [
      { vendor: 'anthropic', model: 'claude-sonnet-5-5', label: 'Claude Sonnet (latest)' },
      { vendor: 'gemini', model: 'gemini-3.8-flash', label: 'Gemini Flash (latest)' },
    ],
    caveat: 'A different family of model answers on fallback.',
  },
  {
    id: 'low',
    title: 'Low tier',
    summary: 'Claude Haiku, falling back to Gemini Flash-Lite.',
    links: [
      { vendor: 'anthropic', model: 'claude-haiku-5-5', label: 'Claude Haiku (latest)' },
      { vendor: 'gemini', model: 'gemini-3.5-flash-lite', label: 'Gemini Flash-Lite (latest)' },
    ],
    caveat: 'A different family of model answers on fallback.',
  },
]

type Step = { label: string; state: 'waiting' | 'running' | 'done' | 'failed'; error?: string }

/**
 * Setting up a deployment's models from a recipe: one chain, its accounts and
 * keys, and a test call through it that says what answered. Everything it
 * makes is ordinary configuration, changed afterwards on the Models page.
 */
export default function PlatformSetup() {
  const [held, setHeld] = useState<HeldCatalog | null>(null)
  const [config, setConfig] = useState<Configured | null>(null)
  const [choice, setChoice] = useState<string>('medium')
  const [keys, setKeys] = useState<Record<string, string>>({})
  const [envs, setEnvs] = useState<Record<string, string>>({})
  const [local, setLocal] = useState({ url: 'http://localhost:11434', model: '' })
  const [steps, setSteps] = useState<Step[] | null>(null)
  const [tested, setTested] = useState<Tested | null>(null)

  useEffect(() => {
    heldCatalog().then(setHeld).catch(() => setHeld(null))
    configured().then(setConfig).catch(() => setConfig(null))
  }, [])

  const recipe = RECIPES.find((r) => r.id === choice)
  const vendors = recipe ? [...new Set(recipe.links.map((l) => l.vendor))].map((v) => VENDORS[v]) : []
  const existing = (v: Vendor): Account | undefined =>
    config?.accounts.find((a) => a.protocol === v.protocol && a.base_url === v.baseUrl)
  const running = steps?.some((s) => s.state === 'running') ?? false

  async function setUp() {
    const plan: Step[] = []
    const run = async (label: string, what: () => Promise<unknown>) => {
      plan.push({ label, state: 'running' })
      setSteps([...plan])
      try {
        const result = await what()
        plan[plan.length - 1].state = 'done'
        setSteps([...plan])
        return result
      } catch (e) {
        plan[plan.length - 1] = { label, state: 'failed', error: e instanceof Error ? e.message : String(e) }
        setSteps([...plan])
        throw e
      }
    }
    setTested(null)
    try {
      const modelIds: string[] = []
      if (recipe) {
        const accounts = new Map<string, Account>()
        for (const v of vendors) {
          let account = existing(v)
          if (!account) {
            const env = envs[v.key]?.trim()
            account = (await run(`Add the ${v.name} account`, () =>
              createAccount({
                name: v.name,
                protocol: v.protocol,
                base_url: v.baseUrl,
                ...(env && !keys[v.key] ? { credential_env: env } : {}),
              }),
            )) as Account
          }
          const key = keys[v.key]?.trim()
          if (key) {
            const target = account
            await run(`Seal the ${v.name} key`, () => sealAccountKey(target, key))
          }
          accounts.set(v.key, account)
        }
        for (const link of recipe.links) {
          const model = (await run(`Add ${link.label}`, () =>
            createModel({ account_id: accounts.get(link.vendor)!.id, model: link.model, tracks: true }),
          )) as { id: string }
          modelIds.push(model.id)
        }
      } else {
        const account = (await run('Add the local account', () =>
          createAccount({ name: 'Local', protocol: 'openai', base_url: local.url.trim() }),
        )) as Account
        const model = (await run(`Add ${local.model}`, () =>
          createModel({
            account_id: account.id,
            model: local.model.trim(),
            facts: { reasoning: true, tool_call: true, temperature: true },
          }),
        )) as { id: string }
        modelIds.push(model.id)
      }
      const name = recipe?.title ?? 'Local'
      const chain = (await run(`Make the chain "${name}"`, () => createChain(name, modelIds))) as { id: string }
      await run('Assign it to assistant and background traffic', async () => {
        await assign('assistant', chain.id)
        await assign('background', chain.id)
      })
      const result = (await run('Make a test call', () => testCall('assistant'))) as Tested
      setTested(result)
    } catch {
      // The failed step says what went wrong; what was made before it stays,
      // and is on the Models page to change or remove.
    }
  }

  const ready =
    recipe !== undefined
      ? vendors.every((v) => existing(v) || keys[v.key]?.trim() || envs[v.key]?.trim())
      : local.url.trim() !== '' && local.model.trim() !== ''

  return (
    <div className="p-6 max-w-3xl space-y-6">
      <div>
        <h1 className="text-2xl font-semibold text-surface-900 dark:text-surface-100">Set up models</h1>
        <p className="mt-2 text-surface-600 dark:text-surface-400">
          A starting point for where this deployment's model requests go. Everything made here is ordinary
          configuration, changed afterwards on the{' '}
          <Link to={paths.platformModels} className="underline underline-offset-2">
            Models page
          </Link>
          .
        </p>
      </div>

      <section className={sectionBox}>
        <h2 className="text-lg font-medium text-surface-900 dark:text-surface-100">1. Catalog</h2>
        <p className="text-sm text-surface-600 dark:text-surface-400">
          {held
            ? `${held.models.toLocaleString()} models from ${held.providers.toLocaleString()} providers, from ${
                held.source === 'shipped' ? 'the copy this release shipped with' : held.source
              }. It says what each model accepts.`
            : 'Reading the catalog...'}
        </p>
      </section>

      <section className={sectionBox}>
        <h2 className="text-lg font-medium text-surface-900 dark:text-surface-100">2. A recipe</h2>
        <div role="radiogroup" aria-label="Recipes" className="grid gap-2 sm:grid-cols-2">
          {[...RECIPES, { id: 'local', title: 'Local', summary: 'A model on your own server, such as ollama or llama.cpp.', links: [], caveat: 'No fallback: when it is down, nothing answers.' }].map(
            (r) => (
              <button
                key={r.id}
                type="button"
                role="radio"
                aria-checked={choice === r.id}
                aria-label={r.title}
                onClick={() => setChoice(r.id)}
                className={`text-left rounded-md border p-3 ${
                  choice === r.id
                    ? 'border-brand-600 bg-brand-50 dark:bg-brand-950'
                    : 'border-surface-200 dark:border-surface-700 hover:bg-surface-50 dark:hover:bg-surface-800'
                }`}
              >
                <p className="font-medium text-surface-900 dark:text-surface-100">{r.title}</p>
                <p className="text-sm text-surface-600 dark:text-surface-400">{r.summary}</p>
                <p className="text-xs text-surface-500 mt-1">{r.caveat}</p>
              </button>
            ),
          )}
        </div>
        <p className="text-sm text-surface-500">
          Or skip this and configure everything by hand on the{' '}
          <Link to={paths.platformModels} className="underline underline-offset-2">
            Models page
          </Link>
          .
        </p>
      </section>

      <section className={sectionBox}>
        <h2 className="text-lg font-medium text-surface-900 dark:text-surface-100">3. Keys</h2>
        {recipe &&
          vendors.map((v) => {
            const has = existing(v)
            return (
              <div key={v.key} className="space-y-2">
                <p className="text-sm font-medium text-surface-900 dark:text-surface-100">{v.name}</p>
                {has ? (
                  <p className="text-sm text-surface-600 dark:text-surface-400">
                    Uses the account already configured, {has.name}
                    {has.credential_id ? ', with its sealed key' : has.credential_env ? `, with ${has.credential_env}` : ''}.
                  </p>
                ) : (
                  <>
                    <label className="block text-sm text-surface-700 dark:text-surface-300">
                      Key
                      <input
                        type="password"
                        autoComplete="off"
                        value={keys[v.key] ?? ''}
                        onChange={(e) => setKeys({ ...keys, [v.key]: e.target.value })}
                        className={field}
                      />
                    </label>
                    <label className="block text-sm text-surface-700 dark:text-surface-300">
                      Or a variable in the gateway's environment
                      <input
                        value={envs[v.key] ?? ''}
                        placeholder={v.env}
                        onChange={(e) => setEnvs({ ...envs, [v.key]: e.target.value })}
                        className={field}
                      />
                    </label>
                  </>
                )}
              </div>
            )
          })}
        {recipe && (
          <p className="text-xs text-surface-500">
            A key typed here is sealed in this browser, usable only at its provider's host and only as that
            account's key.
          </p>
        )}
        {!recipe && (
          <>
            <label className="block text-sm text-surface-700 dark:text-surface-300">
              Base URL
              <input
                type="url"
                value={local.url}
                onChange={(e) => setLocal({ ...local, url: e.target.value })}
                className={field}
              />
            </label>
            <label className="block text-sm text-surface-700 dark:text-surface-300">
              Model, as the server names it
              <input
                value={local.model}
                placeholder="qwen3.5"
                onChange={(e) => setLocal({ ...local, model: e.target.value })}
                className={field}
              />
            </label>
            <p className="text-xs text-surface-500">
              An address inside your network must be one the operator has opened in OUTTURN_INTERNAL_HOSTS.
            </p>
          </>
        )}
      </section>

      <section className={sectionBox}>
        <h2 className="text-lg font-medium text-surface-900 dark:text-surface-100">4. Set up, and a test call</h2>
        <button type="button" className={buttonPrimary} disabled={!ready || running} onClick={() => void setUp()}>
          <Sparkles size={14} aria-hidden />
          Set up {recipe?.title ?? 'Local'}
        </button>
        {steps && (
          <ol className="space-y-1 text-sm">
            {steps.map((s, i) => (
              <li key={i} className="flex items-start gap-2">
                {s.state === 'running' && <Loader2 size={16} className="animate-spin text-surface-500" aria-hidden />}
                {s.state === 'done' && <CheckCircle2 size={16} className="text-brand-600" aria-hidden />}
                {s.state === 'failed' && <CircleAlert size={16} className="text-red-600" aria-hidden />}
                <span className="text-surface-800 dark:text-surface-200">
                  {s.label}
                  {s.error && <span className="block text-red-700 dark:text-red-400">{s.error}</span>}
                </span>
              </li>
            ))}
          </ol>
        )}
        {tested && (
          <div
            role="status"
            className={`rounded-md p-3 text-sm ${
              tested.ok
                ? 'bg-brand-50 dark:bg-brand-950 text-surface-900 dark:text-surface-100'
                : 'bg-red-50 dark:bg-red-950/40 text-red-800 dark:text-red-300'
            }`}
          >
            {tested.ok ? (
              <>
                <p>
                  Answered by <span className="font-medium">{tested.model ?? 'a model'}</span> in{' '}
                  {(tested.elapsed_ms / 1000).toFixed(1)}s: "{tested.reply}"
                </p>
                <Link to={paths.platformModels} className={`${buttonQuiet} mt-2 inline-flex`}>
                  Open the Models page
                </Link>
              </>
            ) : (
              <p>The test call failed: {tested.error}</p>
            )}
          </div>
        )}
      </section>
    </div>
  )
}
