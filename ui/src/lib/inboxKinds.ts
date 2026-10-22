import { ArrowUpCircle, CircleUser, ListChecks, Moon, ShieldQuestion, type LucideIcon } from 'lucide-react'

import type { ActionItem } from './actions'

/**
 * How each kind of item reads in the inbox.
 *
 * One table, so a new producer is one entry here rather than a branch in every
 * component that shows an item. A kind nobody has described still renders, as
 * its own name and whatever reason it carries, rather than as a blank row.
 */
export type ItemKind = 'approval' | 'sleep' | 'model' | 'policy' | 'other'

export type Described = {
  kind: ItemKind
  label: string
  icon: LucideIcon
  /** The one line worth reading from the list. */
  summary: string | null
  /** The conversation it came from, where it names one. */
  sessionId: string | null
}

function text(item: ActionItem, key: string): string | null {
  const value = item.payload[key]
  return typeof value === 'string' && value.trim() !== '' ? value : null
}

/** `approval.charge` reads as "Charge"; `approval.budget_override` as "Budget override". */
function act(kind: string): string {
  const last = kind.split('.').at(-1) ?? kind
  return last.replace(/_/g, ' ').replace(/^./, (c) => c.toUpperCase())
}

export function describeItem(item: ActionItem): Described {
  const sessionId = text(item, 'session_id')
  if (item.kind.startsWith('approval.')) {
    return {
      kind: 'approval',
      label: `${act(item.kind)} to approve`,
      icon: ShieldQuestion,
      summary: text(item, 'reason'),
      sessionId,
    }
  }
  if (item.kind === 'model.upgrade') {
    // Names the model by what the operator called it, and the versions by
    // their ids, which are what the catalog and the page show.
    const name = text(item, 'name')
    const from = text(item, 'from')
    const to = text(item, 'to')
    return {
      kind: 'model',
      label: 'Model upgrade to approve',
      icon: ArrowUpCircle,
      summary: name && from && to ? `${name}: ${from} to ${to}` : text(item, 'reason'),
      sessionId: null,
    }
  }
  if (item.kind === 'policy.accept') {
    // Operations a broad auto-approval policy would cover, published since it
    // was accepted; the policy's page is where they are accepted.
    return {
      kind: 'policy',
      label: 'Operations waiting on a policy',
      icon: ListChecks,
      summary: text(item, 'summary'),
      sessionId: null,
    }
  }
  if (item.kind === 'sleep') {
    return {
      kind: 'sleep',
      label: 'Agent asleep',
      icon: Moon,
      summary: text(item, 'reason'),
      sessionId,
    }
  }
  return {
    kind: 'other',
    label: act(item.kind),
    icon: CircleUser,
    summary: text(item, 'question') ?? text(item, 'summary') ?? text(item, 'reason'),
    sessionId,
  }
}
