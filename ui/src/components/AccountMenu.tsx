import { useEffect, useRef, useState } from 'react'
import { Building2, Check, ChevronsUpDown, LogOut, Monitor, Moon, Sun } from 'lucide-react'

import { useSession, useSessionActions } from '../lib/session'
import type { Theme } from '../lib/theme'
import { useTheme } from '../lib/useTheme'

const themes: { value: Theme; icon: typeof Sun; label: string }[] = [
  { value: 'light', icon: Sun, label: 'Light' },
  { value: 'dark', icon: Moon, label: 'Dark' },
  { value: 'system', icon: Monitor, label: 'System' },
]

/**
 * Initials for the avatar.
 *
 * Two letters from separate words where there are any, otherwise the first two
 * characters, so a single-word name still reads as an avatar rather than a
 * lone letter.
 */
function initials(name: string): string {
  const words = name.trim().split(/\s+/).filter(Boolean)
  if (words.length === 0) return '?'
  if (words.length === 1) return words[0].slice(0, 2).toUpperCase()
  return (words[0][0] + words[words.length - 1][0]).toUpperCase()
}

export default function AccountMenu() {
  const state = useSession()
  const { signOut, switchTenant } = useSessionActions()
  const [theme, setTheme] = useTheme()
  const [open, setOpen] = useState(false)
  const [switching, setSwitching] = useState(false)
  const container = useRef<HTMLDivElement>(null)

  // Dismiss on an outside click or Escape, which is what a menu is expected to
  // do and what a bare button would not give.
  useEffect(() => {
    if (!open) return

    const onPointerDown = (event: PointerEvent) => {
      if (!container.current?.contains(event.target as Node)) setOpen(false)
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setOpen(false)
    }

    document.addEventListener('pointerdown', onPointerDown)
    document.addEventListener('keydown', onKeyDown)
    return () => {
      document.removeEventListener('pointerdown', onPointerDown)
      document.removeEventListener('keydown', onKeyDown)
    }
  }, [open])

  if (state.status !== 'authenticated') return null

  const currentTenantId = state.session.tenant_id
  const current = state.tenants.find((t) => t.tenant_id === currentTenantId)
  const name = state.displayName || 'Account'

  async function selectTenant(tenantId: string) {
    if (tenantId === currentTenantId) {
      setOpen(false)
      return
    }
    setSwitching(true)
    try {
      await switchTenant(tenantId)
      setOpen(false)
    } finally {
      setSwitching(false)
    }
  }

  return (
    <div ref={container} className="relative">
      <button
        onClick={() => setOpen((v) => !v)}
        aria-haspopup="menu"
        aria-expanded={open}
        className="w-full flex items-center gap-2 p-2 rounded-md text-left hover:bg-gray-100 dark:hover:bg-gray-800"
      >
        <span
          aria-hidden
          className="shrink-0 w-8 h-8 rounded-full bg-gray-900 dark:bg-gray-100 text-white dark:text-gray-900 text-xs font-medium flex items-center justify-center"
        >
          {initials(name)}
        </span>
        <span className="flex-1 min-w-0">
          <span className="block text-sm text-gray-900 dark:text-gray-100 truncate">{name}</span>
          <span className="block text-xs text-gray-500 dark:text-gray-400 truncate">
            {current?.name ?? 'No tenant'}
          </span>
        </span>
        <ChevronsUpDown size={14} className="shrink-0 text-gray-400" />
      </button>

      {open && (
        <div
          role="menu"
          className="absolute bottom-full left-0 right-0 mb-1 rounded-md border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-900 shadow-lg overflow-hidden"
        >
          {state.tenants.length > 1 && (
            <div className="p-1 border-b border-gray-200 dark:border-gray-800">
              <p className="px-2 py-1 text-xs font-medium text-gray-500 dark:text-gray-400">
                Tenant
              </p>
              <ul className="max-h-48 overflow-auto">
                {state.tenants.map((tenant) => {
                  const active = tenant.tenant_id === currentTenantId
                  return (
                    <li key={tenant.tenant_id}>
                      <button
                        role="menuitem"
                        disabled={switching}
                        onClick={() => void selectTenant(tenant.tenant_id)}
                        className="w-full flex items-center gap-2 px-2 py-1.5 rounded text-sm text-left text-gray-700 dark:text-gray-300 hover:bg-gray-50 dark:hover:bg-gray-800 disabled:opacity-50"
                      >
                        <Building2 size={14} className="shrink-0 text-gray-400" />
                        <span className="flex-1 min-w-0 truncate">{tenant.name}</span>
                        {active && <Check size={14} className="shrink-0 text-gray-400" />}
                      </button>
                    </li>
                  )
                })}
              </ul>
            </div>
          )}

          <div className="p-1 border-b border-gray-200 dark:border-gray-800">
            <p className="px-2 py-1 text-xs font-medium text-gray-500 dark:text-gray-400">
              Appearance
            </p>
            <div
              role="radiogroup"
              aria-label="Colour theme"
              className="flex gap-0.5 p-0.5 m-1 rounded-md bg-gray-100 dark:bg-gray-800"
            >
              {themes.map(({ value, icon: Icon, label }) => (
                <button
                  key={value}
                  role="radio"
                  aria-checked={theme === value}
                  aria-label={label}
                  title={label}
                  onClick={() => setTheme(value)}
                  className={`flex-1 flex items-center justify-center py-1.5 rounded transition-colors ${
                    theme === value
                      ? 'bg-white dark:bg-gray-950 text-gray-900 dark:text-gray-100 shadow-sm'
                      : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-gray-100'
                  }`}
                >
                  <Icon size={14} />
                </button>
              ))}
            </div>
          </div>

          <div className="p-1">
            <p className="px-2 pb-1 text-xs text-gray-500 dark:text-gray-400 truncate">
              {state.session.roles.join(', ')}
            </p>
            <button
              role="menuitem"
              onClick={signOut}
              className="w-full flex items-center gap-2 px-2 py-1.5 rounded text-sm text-left text-gray-700 dark:text-gray-300 hover:bg-gray-50 dark:hover:bg-gray-800"
            >
              <LogOut size={14} className="shrink-0 text-gray-400" />
              Sign out
            </button>
          </div>
        </div>
      )}
    </div>
  )
}
