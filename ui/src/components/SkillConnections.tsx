import { useCallback, useEffect, useState } from 'react'
import { CheckCircle2, KeyRound, LoaderCircle, Plug, TriangleAlert, Users } from 'lucide-react'

import { ApiError } from '../lib/api'
import {
  connect,
  disconnect,
  listCredentials,
  listRules,
  readServiceAccount,
  replaceKey,
  setOwner,
  testCredential,
  type Credential,
  type EgressRule,
  type Tested,
} from '../lib/credentials'
import { httpFailure } from '../lib/httpFailure'

/**
 * The keys this skill's hosts are reached with, connected from here.
 *
 * A skill names the hosts it reaches and never a credential: the operator
 * publishes one skill, and each workspace connects its own key. So this is
 * where a workspace does that -- paste the key, and it is sealed in this
 * browser to the gateway, stored, and put on the host's rule, with no restart
 * anywhere. Then one GET says whether it works, so a wrong key is a red 401
 * here rather than an agent apologizing for it later.
 *
 * Only hosts the workspace already allows are shown; allowing one is the
 * banner above this.
 */
export default function SkillConnections({
  hosts,
  skillName,
  workspaceId,
  testPath,
  baseUrl,
  reloads = 0,
}: {
  hosts: string[]
  skillName: string
  workspaceId: string
  /** A generated skill's base URL, whose scheme and port a test is sent to:
   *  a rule names only the host. */
  baseUrl?: string | null
  /** Bumped by the page when a host is allowed, so the connection it asks for
   *  next is here without a reload. */
  reloads?: number
  /** A path this skill's key is for, to test with. A generated skill offers
   *  one of its own reads; the root of an API usually answers anybody. */
  testPath?: string
}) {
  const [rules, setRules] = useState<EgressRule[] | null>(null)
  const [credentials, setCredentials] = useState<Credential[]>([])
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(async () => {
    try {
      const [r, c] = await Promise.all([listRules(), listCredentials()])
      setRules(r)
      setCredentials(c)
      setError(null)
    } catch (e) {
      // A member without the authorities sees nothing here rather than an
      // error: connecting keys is an administrator's job.
      if (e instanceof ApiError && e.status === 403) setRules([])
      else setError(e instanceof ApiError ? e.message : 'failed to load connections')
    }
  }, [])

  useEffect(() => {
    void load()
  }, [load, reloads])

  const shown = (rules ?? []).filter((r) => hosts.includes(r.host))
  if (rules === null || shown.length === 0) return null

  return (
    <section className="mt-6" aria-labelledby="skill-connections">
      <h2
        id="skill-connections"
        className="flex items-center gap-2 text-sm font-medium text-surface-800 dark:text-surface-200"
      >
        <Plug size={14} aria-hidden />
        Connections
      </h2>
      <p className="mt-1 text-xs text-surface-600 dark:text-surface-400">
        The key each host is called with. It is sealed in this browser so only the gateway can read
        it, and nobody here can see it again once connected.
      </p>
      {error && (
        <p className="mt-2 text-xs text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}
      <ul className="mt-3 space-y-3">
        {shown.map((rule) => (
          <Connection
            // Keyed on the suggestion too, so one that arrives after the list
            // starts the test path there rather than empty.
            key={`${rule.id}:${testPath ?? ''}`}
            rule={rule}
            credential={credentials.find((c) => c.id === rule.credential) ?? null}
            name={`${skillName} -- ${rule.host}`}
            workspaceId={workspaceId}
            onChanged={load}
            testPath={testPath}
            origin={originOf(baseUrl, rule.host)}
          />
        ))}
      </ul>
    </section>
  )
}

/** One host's key: connect it, test it, replace it, take it off. */
/** What a test proved, in words. Only a refusal without the key beside an
 *  answer with it says the key works; a path that answers anybody says nothing. */
function describeTest(t: Tested): string {
  const said = t.said ? ` It said: ${t.said}` : ''
  const hint = t.hint ? ` ${t.hint}` : ''
  return describeVerdict(t) + said + hint
}

function describeVerdict(t: Tested): string {
  switch (t.verdict) {
    case 'works':
      return `Refused without the key (${t.without_key}) and answered with it (${t.status}): the key works.`
    case 'not_needed':
      return `This path answers without the key too (${t.status}), so it says nothing about the key. Test one the key is needed for.`
    case 'refused':
      return httpFailure(JSON.stringify({ status: t.status })) ?? `The server refused the key (${t.status}).`
    default:
      return (
        (httpFailure(JSON.stringify({ status: t.status })) ?? `The server answered ${t.status}.`) +
        (t.without_key !== null ? ` Without the key it answered ${t.without_key}.` : '')
      )
  }
}

/** A base URL's origin, when it is this host's -- a skill can reach several. */
function originOf(baseUrl: string | null | undefined, host: string): string | undefined {
  if (!baseUrl) return undefined
  try {
    const url = new URL(baseUrl)
    return url.hostname === host ? url.origin : undefined
  } catch {
    return undefined
  }
}

export function Connection({
  rule,
  credential,
  name,
  workspaceId,
  onChanged,
  testPath,
  origin,
}: {
  rule: EgressRule
  credential: Credential | null
  /** What the credential is called if one is connected here. */
  name: string
  workspaceId: string
  onChanged: () => Promise<void>
  testPath?: string
  origin?: string
}) {
  const connected = rule.credential !== null
  // Each person's own: people connect their keys on their account, and the
  // key here, if any, is only for turns nobody started. Its header is fixed,
  // since people's keys are sealed to it.
  const asker = rule.owner === 'asker'
  // Closed until asked for: plenty of hosts take no key at all, and a form
  // open on each of them reads as something left undone.
  const [editing, setEditing] = useState(false)
  // The skill's own word first: an API that takes `api_key` is not helped by
  // a form offering `authorization`.
  const [header, setHeader] = useState(rule.header ?? rule.suggested_header ?? 'authorization')
  const [secret, setSecret] = useState('')
  // A service account's key file pasted as the value is read as one: its key
  // is sealed, and who it signs in as and the scope are bound beside it.
  const serviceAccount = readServiceAccount(secret)
  const [scope, setScope] = useState(credential?.binding.scope ?? '')
  const signsInAs = credential?.binding.kind === 'service_account' ? credential.binding.account : undefined
  const [path, setPath] = useState(testPath ?? '')
  const [busy, setBusy] = useState(false)
  const [tested, setTested] = useState<Tested | null>(null)
  const [error, setError] = useState<string | null>(null)

  async function run(work: () => Promise<unknown>) {
    setBusy(true)
    setError(null)
    try {
      await work()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'that did not work')
    } finally {
      setBusy(false)
    }
  }

  const save = () =>
    run(async () => {
      const value = serviceAccount ? { serviceAccount, scope: scope.trim() } : secret
      const sentHeader = serviceAccount ? 'authorization' : header
      setSecret('')
      if (rule.credential) {
        await replaceKey(rule.credential, workspaceId, rule.host, sentHeader, value)
      } else {
        await connect(rule, workspaceId, name, sentHeader, value)
      }
      setEditing(false)
      setTested(null)
      await onChanged()
    })

  const test = () =>
    run(async () => {
      if (rule.credential) setTested(await testCredential(rule.credential, path, origin))
    })

  const toEveryone = () => {
    if (
      !window.confirm(
        `Use one key for everyone on ${rule.host}? The keys people connected for it are revoked.`,
      )
    )
      return
    void run(async () => {
      await setOwner(rule, 'agent')
      setEditing(false)
      setTested(null)
      await onChanged()
    })
  }

  const toEachPerson = () =>
    void run(async () => {
      await setOwner(rule, 'asker', rule.header ?? header)
      setEditing(false)
      setTested(null)
      await onChanged()
    })

  const remove = () => {
    if (!window.confirm(`Disconnect the key for ${rule.host}? The host stays allowed.`)) return
    void run(async () => {
      await disconnect(rule)
      setEditing(false)
      setTested(null)
      await onChanged()
    })
  }

  return (
    <li className="rounded-md border border-surface-200 dark:border-surface-800 px-3 py-3 text-sm">
      {/* Wraps rather than scrolls: an in-cluster host alone is wider than a
          phone, and the status goes under it when both will not fit. */}
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <span className="min-w-0 break-all font-mono text-surface-900 dark:text-surface-100">{rule.host}</span>
        {asker ? (
          <span className="ml-auto flex items-center gap-1 text-xs text-brand-700 dark:text-brand-400">
            <Users size={12} aria-hidden />
            Each person's own, in <span className="font-mono">{rule.header}</span>
          </span>
        ) : connected ? (
          <span className="ml-auto flex flex-wrap items-center gap-1 text-xs text-green-700 dark:text-green-400">
            <KeyRound size={12} aria-hidden />
            {signsInAs ? (
              <>
                Connected as <span className="break-all font-mono">{signsInAs}</span>
              </>
            ) : (
              <>
                Connected, in <span className="font-mono">{rule.header}</span>
              </>
            )}
            {credential &&
              credential.generation > 1 &&
              ` (key replaced ${credential.generation - 1}×)`}
          </span>
        ) : (
          <span className="ml-auto text-xs text-surface-500 dark:text-surface-400">No key</span>
        )}
      </div>

      {asker && (
        <p className="mt-1 text-xs text-surface-600 dark:text-surface-400">
          People connect their own key under their account, and the service applies its own
          permissions to what each of them asks for. A turn nobody started — a schedule, a webhook
          — sends its agent&apos;s machine user&apos;s key, if the agent names one;{' '}
          {connected
            ? 'otherwise the key below.'
            : 'otherwise it cannot reach this host unless a key is connected below for it.'}
        </p>
      )}

      {editing ? (
        <form
          className="mt-3 space-y-2"
          onSubmit={(e) => {
            e.preventDefault()
            void save()
          }}
        >
          {!serviceAccount && (
          <label className="block text-xs text-surface-700 dark:text-surface-300">
            Header
            <input
              value={asker ? (rule.header ?? header) : header}
              disabled={asker}
              onChange={(e) => setHeader(e.target.value.trim().toLowerCase())}
              className="mt-1 w-full rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 px-2 py-1 font-mono text-xs"
            />
          </label>
          )}
          <label className="block text-xs text-surface-700 dark:text-surface-300">
            Value
            <input
              type="password"
              autoComplete="off"
              value={secret}
              onChange={(e) => setSecret(e.target.value)}
              placeholder={header === 'authorization' ? 'Bearer …' : ''}
              className="mt-1 w-full rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 px-2 py-1 font-mono text-xs"
            />
            {serviceAccount ? (
              <span className="mt-1 block text-surface-500 dark:text-surface-400">
                A service account key for{' '}
                <span className="break-all font-mono">{serviceAccount.account}</span>. Agents
                reach the files shared with that address.
              </span>
            ) : (
            <span className="mt-1 block text-surface-500 dark:text-surface-400">
              The whole header value, exactly as the API wants it.
              {header === 'authorization' && (
                <>
                  {' '}
                  For most APIs that is <span className="font-mono">Bearer</span> followed by the
                  key.
                </>
              )}{' '}
              A service account's key file is accepted as it is.
            </span>
            )}
          </label>
          {serviceAccount && (
            <label className="block text-xs text-surface-700 dark:text-surface-300">
              Scope
              <input
                value={scope}
                onChange={(e) => setScope(e.target.value)}
                placeholder="https://www.googleapis.com/auth/spreadsheets"
                className="mt-1 w-full rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 px-2 py-1 font-mono text-xs"
              />
              <span className="mt-1 block text-surface-500 dark:text-surface-400">
                What tokens from this key may do. Fixed with the key; a different scope means
                connecting it again.
              </span>
            </label>
          )}
          <div className="flex gap-2">
            <button
              type="submit"
              disabled={
                busy || secret === '' || (serviceAccount ? scope.trim() === '' : header === '')
              }
              className="rounded-md bg-brand-600 px-3 py-1.5 text-xs font-medium text-white disabled:opacity-50"
            >
              {connected ? 'Replace key' : 'Connect'}
            </button>
            {
              <button
                type="button"
                onClick={() => setEditing(false)}
                className="rounded-md border border-surface-300 dark:border-surface-700 px-3 py-1.5 text-xs"
              >
                Cancel
              </button>
            }
          </div>
        </form>
      ) : !connected ? (
        <div className="mt-2 flex flex-wrap gap-2">
          <button
            type="button"
            onClick={() => setEditing(true)}
            className="rounded-md border border-surface-300 dark:border-surface-700 px-3 py-1.5 text-xs"
          >
            {asker ? 'Connect a key for turns nobody started' : 'Connect a key'}
          </button>
          {asker ? (
            <button
              type="button"
              onClick={toEveryone}
              disabled={busy}
              className="rounded-md border border-surface-300 dark:border-surface-700 px-3 py-1.5 text-xs"
            >
              Use one key for everyone
            </button>
          ) : (
            <button
              type="button"
              onClick={toEachPerson}
              disabled={busy}
              title="For a service that checks who is asking: each person connects their own key, and it goes only with their conversations."
              className="rounded-md border border-surface-300 dark:border-surface-700 px-3 py-1.5 text-xs"
            >
              Each person connects their own
            </button>
          )}
        </div>
      ) : (
        <div className="mt-3 space-y-2">
          <div className="flex items-center gap-2">
            <label className="flex flex-1 items-center gap-2 text-xs text-surface-700 dark:text-surface-300">
              Test with GET
              <input
                aria-label="Test path"
                value={path}
                onChange={(e) => setPath(e.target.value)}
                placeholder="a path the key is needed for, such as /api/items"
                className="flex-1 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 px-2 py-1 font-mono text-xs"
              />
            </label>
            <button
              type="button"
              onClick={() => void test()}
              disabled={busy || !path.trim()}
              className="rounded-md border border-surface-300 dark:border-surface-700 px-3 py-1.5 text-xs"
            >
              Test
            </button>
          </div>
          {tested && (
            <p
              className={`flex items-center gap-1.5 text-xs ${
                tested.verdict === 'works'
                  ? 'text-green-700 dark:text-green-400'
                  : tested.verdict === 'refused'
                    ? 'text-red-700 dark:text-red-400'
                    : 'text-amber-700 dark:text-amber-400'
              }`}
              role="status"
            >
              {tested.verdict === 'works' ? (
                <CheckCircle2 size={12} aria-hidden />
              ) : (
                <TriangleAlert size={12} aria-hidden />
              )}
              {describeTest(tested)}
              {tested.fingerprint && (
                <span
                  className="ml-1 font-mono text-surface-500 dark:text-surface-400"
                  title="Identifies the key the gateway sent without revealing it. It stays the same until the key is replaced or the gateway's sealing key is rotated."
                >
                  · key {tested.fingerprint}
                </span>
              )}
            </p>
          )}
          <div className="flex gap-2">
            <button
              type="button"
              onClick={() => setEditing(true)}
              className="rounded-md border border-surface-300 dark:border-surface-700 px-3 py-1.5 text-xs"
            >
              Replace key
            </button>
            <button
              type="button"
              onClick={remove}
              disabled={busy}
              className="rounded-md border border-red-300 dark:border-red-900 px-3 py-1.5 text-xs text-red-700 dark:text-red-400"
            >
              Disconnect
            </button>
            <button
              type="button"
              onClick={asker ? toEveryone : toEachPerson}
              disabled={busy}
              className="rounded-md border border-surface-300 dark:border-surface-700 px-3 py-1.5 text-xs"
            >
              {asker ? 'Use one key for everyone' : 'Each person connects their own'}
            </button>
          </div>
        </div>
      )}

      {busy && (
        <p className="mt-2 flex items-center gap-1 text-xs text-surface-500">
          <LoaderCircle size={12} className="animate-spin" aria-hidden /> Working…
        </p>
      )}
      {error && (
        <p className="mt-2 text-xs text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}
    </li>
  )
}
