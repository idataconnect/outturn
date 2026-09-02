import { useState } from 'react'
import { Building2, Check, ChevronsUpDown } from 'lucide-react'

import { useSession, useSessionActions } from '../lib/session'

export default function TenantSwitcher() {
  const state = useSession()
  const { switchTenant } = useSessionActions()
  const [open, setOpen] = useState(false)
  const [pending, setPending] = useState(false)

  if (state.status !== 'authenticated') return null

  const currentTenantId = state.session.tenant_id
  const current = state.tenants.find((t) => t.tenant_id === currentTenantId)

  async function select(tenantId: string) {
    setOpen(false)
    if (tenantId === currentTenantId) return
    setPending(true)
    try {
      await switchTenant(tenantId)
    } finally {
      setPending(false)
    }
  }

  return (
    <div className="relative">
      <button
        onClick={() => setOpen((v) => !v)}
        disabled={pending || state.tenants.length < 2}
        aria-haspopup="listbox"
        aria-expanded={open}
        className="w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm text-left text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-800 disabled:hover:bg-transparent disabled:cursor-default"
      >
        <Building2 size={16} className="shrink-0 text-gray-400" />
        <span className="flex-1 min-w-0 truncate">{current?.name ?? 'No tenant'}</span>
        {state.tenants.length > 1 && (
          <ChevronsUpDown size={14} className="shrink-0 text-gray-400" />
        )}
      </button>

      {open && (
        <ul
          role="listbox"
          className="absolute z-10 left-0 right-0 mt-1 rounded-md border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-900 shadow-lg max-h-64 overflow-auto"
        >
          {state.tenants.map((tenant) => {
            const active = tenant.tenant_id === currentTenantId
            return (
              <li key={tenant.tenant_id}>
                <button
                  role="option"
                  aria-selected={active}
                  onClick={() => void select(tenant.tenant_id)}
                  className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left text-gray-700 dark:text-gray-300 hover:bg-gray-50 dark:hover:bg-gray-800"
                >
                  <span className="flex-1 min-w-0 truncate">{tenant.name}</span>
                  {active && <Check size={14} className="shrink-0 text-gray-400" />}
                </button>
              </li>
            )
          })}
        </ul>
      )}
    </div>
  )
}
