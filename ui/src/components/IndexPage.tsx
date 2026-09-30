import type { ReactNode } from 'react'
import { Link } from 'react-router'
import { ChevronRight, Plus, Search } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'

/**
 * The parts every index page is built from: Skills, Users, Roles, Workspaces.
 *
 * Index pages are for things somebody configures and then leaves -- a titled
 * page, a sentence saying what the page is for, a list, each row opening a page
 * of its own. Things somebody works *in*, moving between them all day, get a
 * list beside the open item instead (Agents, Sessions). That is the rule, and
 * these components are what keep the index half of it looking like one app:
 * each page used to build its own header and rows, and they had drifted -- one
 * row had no icon, one had a delete button on it, one page's button wrapped.
 */

/** Title, the sentence under it, and the one action, which never wraps. */
export function PageHeader({
  title,
  description,
  action,
}: {
  title: string
  description: ReactNode
  action?: { to: string; label: string }
}) {
  return (
    <div className="flex items-start justify-between gap-4">
      <div className="min-w-0">
        <h1 className="text-2xl font-semibold text-surface-900 dark:text-surface-100">{title}</h1>
        <p className="mt-2 text-surface-600 dark:text-surface-400">{description}</p>
      </div>
      {action && (
        <Link
          to={action.to}
          className="shrink-0 whitespace-nowrap flex items-center gap-2 px-4 py-2 rounded-md bg-brand-700 hover:bg-brand-600 dark:bg-brand-600 dark:hover:bg-brand-500 text-white text-sm font-medium"
        >
          <Plus size={16} aria-hidden />
          {action.label}
        </Link>
      )}
    </div>
  )
}

/** Narrows a list by what its rows say. Shown only once there is a list. */
export function FilterBox({
  value,
  onChange,
  label,
}: {
  value: string
  onChange: (value: string) => void
  label: string
}) {
  return (
    <label className="relative mt-6 block max-w-xs">
      <span className="sr-only">{label}</span>
      <Search
        size={14}
        aria-hidden
        className="absolute left-2 top-1/2 -translate-y-1/2 text-surface-400"
      />
      <input
        type="search"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={label}
        className="w-full pl-7 pr-2 py-1.5 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 text-sm text-surface-900 dark:text-surface-100 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500"
      />
    </label>
  )
}

/**
 * The bordered group the rows sit in, or what to say instead of rows.
 *
 * `empty` is shown when there is nothing at all, and a separate line when a
 * filter matched nothing -- "no users yet" to somebody who typed a name would
 * read as the users having gone.
 */
export function RecordList({
  loading,
  count,
  shown,
  empty,
  children,
}: {
  loading: boolean
  /** How many there are. */
  count: number
  /** How many the filter left. */
  shown: number
  empty: ReactNode
  children: ReactNode
}) {
  const message = (text: ReactNode) => (
    <p className="p-4 text-sm text-surface-600 dark:text-surface-400">{text}</p>
  )
  return (
    <div className="mt-4 rounded-lg border border-surface-200 dark:border-surface-800 bg-white dark:bg-surface-900 overflow-hidden">
      {loading ? (
        message('Loading…')
      ) : count === 0 ? (
        message(empty)
      ) : shown === 0 ? (
        message('Nothing matches.')
      ) : (
        <ul className="divide-y divide-surface-200 dark:divide-surface-800">{children}</ul>
      )}
    </div>
  )
}

/**
 * One record, clickable across its whole width, ending in a chevron.
 *
 * The chevron is what says a row opens something; before it only the pointer
 * changing did, and only over the text. Actions -- deleting above all -- live
 * on the record's own page beside what they act on, never on the row, where a
 * click meant to open the record lands on a bin instead.
 */
export function RecordRow({
  to,
  icon: Icon,
  title,
  badges,
  aside,
  children,
}: {
  to: string
  icon: LucideIcon
  title: string
  /** Small facts beside the title: counts, "retired", "from the operator". */
  badges?: ReactNode
  /** Something worth seeing from the list without opening the record. */
  aside?: ReactNode
  /** The lines under the title. */
  children?: ReactNode
}) {
  return (
    <li>
      <Link
        to={to}
        className="group flex items-start gap-4 p-4 hover:bg-surface-50 dark:hover:bg-surface-800/60"
      >
        <Icon size={16} className="mt-1 shrink-0 text-surface-400" aria-hidden />
        <div className="flex-1 min-w-0">
          <p className="text-sm font-medium text-surface-900 dark:text-surface-100 truncate">
            {title}
            {badges}
          </p>
          {children}
        </div>
        {aside && <div className="shrink-0 self-center">{aside}</div>}
        <ChevronRight
          size={16}
          aria-hidden
          className="shrink-0 self-center text-surface-300 group-hover:text-surface-500 dark:text-surface-600 dark:group-hover:text-surface-400"
        />
      </Link>
    </li>
  )
}

/** A fact beside a row's title, in the quieter type of its lines. */
export function Badge({ children }: { children: ReactNode }) {
  return (
    <span className="ml-2 text-xs font-normal text-surface-600 dark:text-surface-400">
      {children}
    </span>
  )
}
