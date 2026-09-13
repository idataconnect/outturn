import { useLocation, useNavigate } from 'react-router'
import { useEffect, useRef, useState } from 'react'
import { Building2, Check, ChevronsUpDown, LogOut } from 'lucide-react'

import { useSession, useSessionActions } from '../lib/session'
import ThemeToggle from './ThemeToggle'

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

export default function AccountMenu({
  /** On a collapsed rail only the avatar fits; the name moves to a tooltip. */
  collapsed = false,
}: {
  collapsed?: boolean
} = {}) {
  const state = useSession()
  const { signOut, switchWorkspace } = useSessionActions()
  const [open, setOpen] = useState(false)
  const [switching, setSwitching] = useState(false)
  const container = useRef<HTMLDivElement>(null)
  const navigate = useNavigate()
  const location = useLocation()

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

  const currentWorkspaceId = state.session.workspace_id
  const current = state.workspaces.find((t) => t.workspace_id === currentWorkspaceId)
  const name = state.displayName || 'Account'

  async function selectWorkspace(workspaceId: string) {
    if (workspaceId === currentWorkspaceId) {
      setOpen(false)
      return
    }
    setSwitching(true)
    try {
      await switchWorkspace(workspaceId)
      setOpen(false)
      // Whatever was open belonged to the old workspace: an agent being edited,
      // a conversation, a user. Go back to the section it was in, which the
      // new workspace has a version of, rather than leave a record on screen
      // that the new token cannot even read.
      const section = `/${location.pathname.split('/')[1] ?? ''}`
      void navigate(section, { replace: true })
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
        title={collapsed ? `${name} -- ${current?.name ?? 'No workspace'}` : undefined}
        aria-label={collapsed ? name : undefined}
        className={`w-full flex items-center gap-2 p-2 rounded-md text-left hover:bg-surface-100 dark:hover:bg-surface-800 ${
          collapsed ? 'justify-center' : ''
        }`}
      >
        <span
          aria-hidden
          className="shrink-0 w-8 h-8 rounded-full bg-gradient-to-br from-brand-600 to-orange-500 text-white text-xs font-medium flex items-center justify-center"
        >
          {initials(name)}
        </span>
        {!collapsed && (
          <>
            <span className="flex-1 min-w-0">
              <span className="block text-sm text-surface-900 dark:text-surface-100 truncate">
                {name}
              </span>
              <span className="block text-xs text-surface-600 dark:text-surface-400 truncate">
                {current?.name ?? 'No workspace'}
              </span>
            </span>
            <ChevronsUpDown size={14} className="shrink-0 text-surface-400" />
          </>
        )}
      </button>

      {open && (
        <div
          role="menu"
          className={`absolute bottom-full mb-1 rounded-md border border-surface-200 dark:border-surface-700 bg-white dark:bg-surface-900 shadow-lg overflow-hidden ${
            collapsed ? 'left-0 w-56' : 'left-0 right-0'
          }`}
        >
          {state.workspaces.length > 1 && (
            <div className="p-1 border-b border-surface-200 dark:border-surface-800">
              <p className="px-2 py-1 text-xs font-medium text-surface-600 dark:text-surface-400">
                Workspace
              </p>
              <ul className="max-h-48 overflow-auto">
                {state.workspaces.map((workspace) => {
                  const active = workspace.workspace_id === currentWorkspaceId
                  return (
                    <li key={workspace.workspace_id}>
                      <button
                        role="menuitem"
                        disabled={switching}
                        onClick={() => void selectWorkspace(workspace.workspace_id)}
                        className="w-full flex items-center gap-2 px-2 py-1.5 rounded text-sm text-left text-surface-700 dark:text-surface-300 hover:bg-surface-50 dark:hover:bg-surface-800 disabled:opacity-50"
                      >
                        <Building2 size={14} className="shrink-0 text-surface-400" />
                        <span className="flex-1 min-w-0 truncate">{workspace.name}</span>
                        {active && <Check size={14} className="shrink-0 text-surface-400" />}
                      </button>
                    </li>
                  )
                })}
              </ul>
            </div>
          )}

          <div className="p-1 border-b border-surface-200 dark:border-surface-800">
            <p className="px-2 py-1 text-xs font-medium text-surface-600 dark:text-surface-400">
              Appearance
            </p>
            <ThemeToggle />
          </div>

          <div className="p-1">
            <p className="px-2 pb-1 text-xs text-surface-600 dark:text-surface-400 truncate">
              {state.session.roles.join(', ')}
            </p>
            <button
              role="menuitem"
              onClick={signOut}
              className="w-full flex items-center gap-2 px-2 py-1.5 rounded text-sm text-left text-surface-700 dark:text-surface-300 hover:bg-surface-50 dark:hover:bg-surface-800"
            >
              <LogOut size={14} className="shrink-0 text-surface-400" />
              Sign out
            </button>
          </div>
        </div>
      )}
    </div>
  )
}
