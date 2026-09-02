import { useState } from 'react'
import { Building2, LogIn } from 'lucide-react'

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
      signIn(result.display_name, result.tenants)
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
    <div className="min-h-screen flex items-center justify-center bg-gray-50 dark:bg-gray-950 p-6">
      <div className="w-full max-w-sm">
        <div className="flex flex-col items-center gap-3">
          <img src="/favicon.svg" alt="" className="w-16 h-16" />
          <h1 className="text-2xl font-semibold text-gray-900 dark:text-gray-100">outturn</h1>
        </div>

        {choices ? (
          <div className="mt-8 rounded-lg border border-gray-200 dark:border-gray-800 bg-white dark:bg-gray-900 overflow-hidden">
            <p className="p-4 text-sm text-gray-600 dark:text-gray-400 border-b border-gray-200 dark:border-gray-800">
              Choose a tenant
            </p>
            <ul className="divide-y divide-gray-200 dark:divide-gray-800">
              {choices.map((tenant) => (
                <li key={tenant.tenant_id}>
                  <button
                    onClick={() => void submit(tenant.tenant_id)}
                    disabled={pending}
                    className="w-full flex items-center gap-3 p-4 text-left hover:bg-gray-50 dark:hover:bg-gray-800/50 disabled:opacity-50"
                  >
                    <Building2 size={16} className="text-gray-400 shrink-0" />
                    <span className="flex-1 min-w-0">
                      <span className="block text-sm text-gray-900 dark:text-gray-100 truncate">
                        {tenant.name}
                      </span>
                      <span className="block text-xs text-gray-500 dark:text-gray-400 truncate">
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
            className="mt-8 space-y-4 p-6 rounded-lg border border-gray-200 dark:border-gray-800 bg-white dark:bg-gray-900"
          >
            <label className="block">
              <span className="block text-sm text-gray-700 dark:text-gray-300 mb-1">Email</span>
              <input
                type="email"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                required
                autoFocus
                autoComplete="username"
                className="w-full px-3 py-2 rounded-md border border-gray-300 dark:border-gray-700 bg-white dark:bg-gray-950 text-gray-900 dark:text-gray-100"
              />
            </label>
            <label className="block">
              <span className="block text-sm text-gray-700 dark:text-gray-300 mb-1">Password</span>
              <input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                required
                autoComplete="current-password"
                className="w-full px-3 py-2 rounded-md border border-gray-300 dark:border-gray-700 bg-white dark:bg-gray-950 text-gray-900 dark:text-gray-100"
              />
            </label>
            <button
              type="submit"
              disabled={pending}
              className="w-full flex items-center justify-center gap-2 px-4 py-2 rounded-md bg-gray-900 dark:bg-gray-100 text-white dark:text-gray-900 text-sm font-medium disabled:opacity-50"
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
