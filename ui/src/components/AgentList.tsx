import { useState } from 'react'
import { Link } from 'react-router'
import { Search } from 'lucide-react'

import type { Agent } from '../lib/chat'

/** Whether an agent answers to what was typed: name, slug or description. */
function matches(agent: Agent, query: string) {
  const q = query.trim().toLowerCase()
  if (!q) return true
  return [agent.name, agent.slug, agent.description].some((s) => s.toLowerCase().includes(q))
}

/**
 * Agents with a filter box over them, each a link.
 *
 * Filtered here rather than by the API: a roster is configuration, a
 * workspace's worth at most, and all of it is already loaded to draw the list.
 * Where each one leads is the caller's, because the same list opens an agent's
 * page from the directory and starts a conversation from a new chat.
 */
export default function AgentList({
  agents,
  href,
  empty,
  autoFocus,
  current,
}: {
  agents: Agent[]
  href: (agent: Agent) => string
  /** Said when there are no agents at all, as opposed to none matching. */
  empty: React.ReactNode
  autoFocus?: boolean
  /** The agent to mark as the one open, if any. Said by the page rather than
   *  matched from the route: the page may be showing an agent the URL does
   *  not name, and links that differ only by query string all match. */
  current?: string
}) {
  const [query, setQuery] = useState('')
  const shown = agents.filter((a) => matches(a, query))

  if (agents.length === 0) return <>{empty}</>

  return (
    <div className="flex flex-col min-h-0">
      <label className="relative block">
        <span className="sr-only">Filter agents</span>
        <Search
          size={14}
          aria-hidden
          className="absolute left-2 top-1/2 -translate-y-1/2 text-surface-400"
        />
        <input
          type="search"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Filter agents"
          autoFocus={autoFocus}
          className="w-full pl-7 pr-2 py-1.5 rounded-md border border-surface-300 dark:border-surface-700 bg-white dark:bg-surface-800 text-sm text-surface-900 dark:text-surface-100 focus:outline-none focus:ring-2 focus:ring-brand-500/40 focus:border-brand-500"
        />
      </label>
      <ul className="mt-2 space-y-1 overflow-auto">
        {shown.map((agent) => (
          <li key={agent.id}>
            <Link
              to={href(agent)}
              title={agent.description || agent.name}
              aria-current={agent.id === current ? 'page' : undefined}
              className={`block px-2 py-1.5 rounded-md text-sm ${
                agent.id === current
                  ? 'bg-brand-50 dark:bg-brand-950 text-brand-800 dark:text-brand-200'
                  : 'text-surface-700 dark:text-surface-300 hover:bg-surface-50 dark:hover:bg-surface-800/50'
              }`}
            >
              <span className="block truncate">{agent.name}</span>
              {agent.description && (
                <span className="block truncate text-xs text-surface-400 dark:text-surface-500">
                  {agent.description}
                </span>
              )}
            </Link>
          </li>
        ))}
        {shown.length === 0 && (
          <li className="px-2 py-1.5 text-xs text-surface-500 dark:text-surface-400">
            No agent matches “{query.trim()}”.
          </li>
        )}
      </ul>
    </div>
  )
}
