import { useState } from 'react'
import { Building2, LogIn } from 'lucide-react'

import ThemeToggle from '../components/ThemeToggle'
import { ApiError, NetworkError, api } from '../lib/api'
import { useSessionActions, type TenantMembership } from '../lib/session'

type LoginResponse =
  | {
      status: 'select_tenant'
      user_id: string
      display_name: string
      tenants: TenantMembership[]
    }
  | {
      status: 'authenticated'
      user_id: string
      display_name: string
      tenant_id: string
      roles: string[]
      tenants: TenantMembership[]
    }

export default function Login() {
  const { signIn } = useSessionActions()
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [pending, setPending] = useState(false)
  const [error, setError] = useState<string | null>(null)
  // Populated when credentials check out but the account spans several tenants.
  const [choices, setChoices] = useState<TenantMembership[] | null>(null)

  async function submit(tenantId?: string) {
    setPending(true)
    setError(null)
    try {
      const result = await api<LoginResponse>('/v1/login', {
        method: 'POST',
        body: JSON.stringify({ email, password, tenant_id: tenantId ?? null }),
      })

      if (result.status === 'select_tenant') {
        if (result.tenants.length === 0) {
          setError('This account has no tenant access.')
          return
        }
        // A single tenant needs no picker — go straight in.
        if (result.tenants.length === 1) {
          await submit(result.tenants[0].tenant_id)
          return
        }
        setChoices(result.tenants)
        return
      }

      // The token arrived as an HttpOnly cookie, not in this response.
      signIn()
    } catch (e) {
      setError(
        e instanceof NetworkError
          ? e.message
          : e instanceof ApiError && e.status === 401
            ? 'Incorrect email or password.'
            : e instanceof ApiError
              ? e.message
              : 'Sign in failed.',
      )
    } finally {
      setPending(false)
    }
  }

  return (
    <div className="min-h-screen flex items-center justify-center bg-surface-50 dark:bg-surface-950 p-6">
      <div className="w-full max-w-sm">
        <div className="flex justify-end mb-4">
          <div className="w-28">
            <ThemeToggle />
          </div>
        </div>
        <div className="flex flex-col items-center gap-3">
          <img src="/favicon.svg" alt="" className="w-16 h-16" />
          <h1 className="text-2xl font-semibold text-surface-900 dark:text-surface-100">outturn</h1>
        </div>

        {choices ? (
          <div className="mt-8 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 overflow-hidden">
            <p className="p-4 text-sm text-surface-600 dark:text-surface-400 border-b border-surface-200 dark:border-surface-800">
              Choose a tenant
            </p>
            <ul className="divide-y divide-surface-200 dark:divide-surface-800">
              {choices.map((tenant) => (
                <li key={tenant.tenant_id}>
                  <button
                    onClick={() => void submit(tenant.tenant_id)}
                    disabled={pending}
                    className="w-full flex items-center gap-3 p-4 text-left hover:bg-surface-50 dark:hover:bg-surface-800/50 disabled:opacity-50"
                  >
                    <Building2 size={16} className="text-surface-400 shrink-0" />
                    <span className="flex-1 min-w-0">
                      <span className="block text-sm text-surface-900 dark:text-surface-100 truncate">
                        {tenant.name}
                      </span>
                      <span className="block text-xs text-surface-600 dark:text-surface-400 truncate">
                        {tenant.roles.join(', ') || 'system access'}
                      </span>
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          </div>
        ) : (
          <form
            onSubmit={(e) => {
              e.preventDefault()
              void submit()
            }}
            className="mt-8 space-y-4 p-6 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900"
          >
            <label className="block">
              <span className="block text-sm text-surface-700 dark:text-surface-300 mb-1">Email</span>
              <input
                type="email"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                required
                autoFocus
                autoComplete="username"
                className="w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100"
              />
            </label>
            <label className="block">
              <span className="block text-sm text-surface-700 dark:text-surface-300 mb-1">Password</span>
              <input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                required
                autoComplete="current-password"
                className="w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-950 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100"
              />
            </label>
            <button
              type="submit"
              disabled={pending}
              className="w-full flex items-center justify-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium disabled:opacity-50"
            >
              <LogIn size={16} />
              {pending ? 'Signing in…' : 'Sign in'}
            </button>
          </form>
        )}

        {error && (
          <p className="mt-4 text-sm text-red-600 dark:text-red-400 text-center" role="alert">
            {error}
          </p>
        )}
      </div>
    </div>
  )
}
