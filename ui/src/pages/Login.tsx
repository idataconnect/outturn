import { useState } from 'react'
import { Building2, LogIn } from 'lucide-react'

import ThemeToggle from '../components/ThemeToggle'
import { ApiError, NetworkError, api } from '../lib/api'
import { useSessionActions, type WorkspaceMembership } from '../lib/session'
import Logo from '../components/Logo'
import { productName } from '../lib/brand'

type LoginResponse =
  | {
      status: 'select_workspace'
      user_id: string
      display_name: string
      workspaces: WorkspaceMembership[]
    }
  | {
      status: 'authenticated'
      user_id: string
      display_name: string
      workspace_id: string
      roles: string[]
      workspaces: WorkspaceMembership[]
    }

export default function Login() {
  const { signIn } = useSessionActions()
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [pending, setPending] = useState(false)
  const [error, setError] = useState<string | null>(null)
  // Populated when credentials check out but the account spans several workspaces.
  const [choices, setChoices] = useState<WorkspaceMembership[] | null>(null)

  async function submit(workspaceId?: string) {
    setPending(true)
    setError(null)
    try {
      const result = await api<LoginResponse>('/v1/login', {
        method: 'POST',
        body: JSON.stringify({ email, password, workspace_id: workspaceId ?? null }),
      })

      if (result.status === 'select_workspace') {
        if (result.workspaces.length === 0) {
          setError('This account has no workspace access.')
          return
        }
        // A single workspace needs no picker — go straight in.
        if (result.workspaces.length === 1) {
          await submit(result.workspaces[0].workspace_id)
          return
        }
        setChoices(result.workspaces)
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
    <div className="min-h-screen flex items-center justify-center bg-surface-50 dark:bg-surface-900 p-6">
      <div className="w-full max-w-sm">
        <div className="flex justify-end mb-4">
          <div className="w-28">
            <ThemeToggle />
          </div>
        </div>
        <div className="flex flex-col items-center gap-3">
          <Logo className="w-16 h-16" />
          <h1 className="text-2xl font-display font-semibold text-surface-900 dark:text-surface-100">
            {productName}
          </h1>
        </div>

        {choices ? (
          <div className="mt-8 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 overflow-hidden">
            <p className="p-4 text-sm text-surface-600 dark:text-surface-400 border-b border-surface-200 dark:border-surface-800">
              Choose a workspace
            </p>
            <ul className="divide-y divide-surface-200 dark:divide-surface-800">
              {choices.map((workspace) => (
                <li key={workspace.workspace_id}>
                  <button
                    onClick={() => void submit(workspace.workspace_id)}
                    disabled={pending}
                    className="w-full flex items-center gap-3 p-4 text-left hover:bg-surface-50 dark:hover:bg-surface-800/50 disabled:opacity-50"
                  >
                    <Building2 size={16} className="text-surface-400 shrink-0" />
                    <span className="flex-1 min-w-0">
                      <span className="block text-sm text-surface-900 dark:text-surface-100 truncate">
                        {workspace.name}
                      </span>
                      <span className="block text-xs text-surface-600 dark:text-surface-400 truncate">
                        {workspace.roles.join(', ') || 'system access'}
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
                className="w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100"
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
                className="w-full px-3 py-2 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500 text-surface-900 dark:text-surface-100"
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
