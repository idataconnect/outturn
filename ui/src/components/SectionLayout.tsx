import { NavLink, Outlet } from 'react-router'
import type { LucideIcon } from 'lucide-react'

export type SectionItem = {
  to: string
  label: string
  icon: LucideIcon
  /** Whether this reader may open it. Items they may not are not offered. */
  allowed: boolean
  /** Active only on this exact path, for a section's own landing page. */
  end?: boolean
}

/**
 * A section of the app with its own navigation: Settings, Platform.
 *
 * The pages in it are reached rarely and together, so they share one entry in
 * the main navigation and are chosen here instead -- a column beside the page
 * on a wide screen, a row above it on a narrow one.
 */
export default function SectionLayout({ title, items }: { title: string; items: SectionItem[] }) {
  const shown = items.filter((item) => item.allowed)
  return (
    <div className="flex flex-col md:flex-row">
      <aside className="shrink-0 border-b border-surface-200 px-4 pt-6 dark:border-surface-800 md:w-52 md:border-b-0 md:border-r md:pb-6 md:min-h-screen">
        <h2 className="px-2 text-xs font-medium uppercase tracking-wide text-surface-500 dark:text-surface-400">
          {title}
        </h2>
        <nav aria-label={title} className="mt-2 flex gap-1 overflow-x-auto pb-2 md:flex-col md:pb-0">
          {shown.map(({ to, label, icon: Icon, end }) => (
            <NavLink
              key={to}
              to={to}
              end={end}
              className={({ isActive }) =>
                `flex shrink-0 items-center gap-2 rounded-md px-2 py-1.5 text-sm ${
                  isActive
                    ? 'bg-brand-50 font-medium text-brand-800 dark:bg-brand-950 dark:text-brand-200'
                    : 'text-surface-600 hover:bg-surface-100 dark:text-surface-400 dark:hover:bg-surface-800/50'
                }`
              }
            >
              <Icon size={15} className="shrink-0" aria-hidden />
              {label}
            </NavLink>
          ))}
        </nav>
      </aside>
      <div className="min-w-0 flex-1">
        <Outlet />
      </div>
    </div>
  )
}
