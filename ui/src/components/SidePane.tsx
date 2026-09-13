import { useCallback, useEffect, useState, type ComponentType } from 'react'
import { X } from 'lucide-react'

import { readFlag, readOneOf, storeFlag, storeOneOf } from '../lib/layout'
import { useBreakpoint } from '../lib/useBreakpoint'

/**
 * One thing the pane can show. `render` is a component rather than an element
 * so a tab's work only happens while it is the one on screen: a closed pane
 * fetches nothing.
 */
export type PaneTab = {
  id: string
  label: string
  icon: ComponentType<{ size?: number; className?: string; 'aria-hidden'?: boolean }>
  render: ComponentType
  /** Withheld when absent, so a tab can require an authority. */
  available?: boolean
}

/**
 * A rail of icons and the panel one of them opens, after VS Code's right pane.
 *
 * The rail is always there, at every width. Closing the panel leaves the icons
 * behind rather than hiding what the pane can do -- a tab nobody can see is a
 * tab nobody opens -- and clicking the open tab's icon closes it again, so one
 * control both opens and shuts.
 *
 * Only the panel's *presentation* follows the width. With room it sits beside
 * the thread and pushes it narrower; without room it floats above as an
 * overlay, because a 288px panel beside a 390px screen leaves nothing to read.
 */
export default function SidePane({
  tabs,
  storageKey,
}: {
  tabs: PaneTab[]
  /** Distinguishes one pane's remembered state from another's. */
  storageKey: string
}) {
  const breakpoint = useBreakpoint()
  const shown = tabs.filter((t) => t.available !== false)
  const ids = shown.map((t) => t.id)

  const [active, setActive] = useState(() =>
    readOneOf(`${storageKey}.tab`, ids, ids[0] ?? ''),
  )
  // Only a desktop has room to start with a panel already open; anywhere else
  // the reader asks for it.
  const [open, setOpen] = useState(() =>
    readFlag(`${storageKey}.open`, currentlyRoomy()),
  )

  const choose = useCallback(
    (id: string) => {
      // The icon of the tab already showing is the way to put it away again.
      if (id === active && open) {
        setOpen(false)
        storeFlag(`${storageKey}.open`, false)
        return
      }
      setActive(id)
      storeOneOf(`${storageKey}.tab`, id)
      setOpen(true)
      storeFlag(`${storageKey}.open`, true)
    },
    [active, open, storageKey],
  )

  const close = useCallback(() => {
    setOpen(false)
    storeFlag(`${storageKey}.open`, false)
  }, [storageKey])

  // An overlay traps the reader until it is dismissed, so Escape must work.
  useEffect(() => {
    if (!open || breakpoint === 'desktop') return
    function onKey(e: KeyboardEvent) {
      if (e.key === 'Escape') close()
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [open, breakpoint, close])

  // A tab that went away with someone's authorities must not stay selected.
  useEffect(() => {
    if (ids.length > 0 && !ids.includes(active)) setActive(ids[0])
  }, [ids, active])

  if (shown.length === 0) return null

  const current = shown.find((t) => t.id === active) ?? shown[0]
  const Panel = current.render
  const floating = breakpoint !== 'desktop'

  return (
    <>
      {open && floating && (
        <div
          className="fixed inset-0 z-20 bg-black/30"
          onClick={close}
          aria-hidden
        />
      )}

      <div className="flex h-full shrink-0">
        {open && (
          <section
            aria-label={current.label}
            className={`flex flex-col bg-white dark:bg-surface-900 border-l border-surface-200 dark:border-surface-800 ${
              floating ? 'fixed inset-y-0 right-12 z-30 w-72 max-w-[calc(100vw-3rem)] shadow-xl' : 'w-72'
            }`}
          >
            <div className="flex items-center justify-between gap-2 px-3 py-2 border-b border-surface-200 dark:border-surface-800">
              {/* The name lives here rather than on the rail: the rail has no
                  room for it past a few tabs, and this is where the reader
                  already is once the panel is open. */}
              <h2 className="text-xs font-medium text-surface-600 dark:text-surface-400">
                {current.label}
              </h2>
              <button
                type="button"
                onClick={close}
                aria-label={`Close ${current.label}`}
                className="p-1 rounded text-surface-400 hover:text-surface-900 dark:hover:text-surface-100"
              >
                <X size={16} aria-hidden />
              </button>
            </div>
            <div className="flex-1 min-h-0 overflow-hidden">
              <Panel />
            </div>
          </section>
        )}

        <div
          role="tablist"
          aria-orientation="vertical"
          aria-label="Side panels"
          className="relative z-30 flex flex-col items-center gap-1 w-12 shrink-0 py-2 border-l border-surface-200 dark:border-surface-800 bg-surface-50 dark:bg-surface-900"
        >
          {shown.map((tab) => {
            const Icon = tab.icon
            const selected = open && tab.id === current.id
            return (
              <button
                key={tab.id}
                type="button"
                role="tab"
                aria-selected={selected}
                // The panel is a sibling, not a descendant, so it is named
                // rather than pointed at with aria-controls.
                title={tab.label}
                aria-label={tab.label}
                onClick={() => choose(tab.id)}
                className={`p-2 rounded-md transition-colors ${
                  selected
                    ? 'bg-brand-50 dark:bg-brand-950 text-brand-700 dark:text-brand-300'
                    : 'text-surface-500 dark:text-surface-400 hover:bg-surface-100 dark:hover:bg-surface-800'
                }`}
              >
                <Icon size={18} aria-hidden />
              </button>
            )
          })}
        </div>
      </div>
    </>
  )
}

function currentlyRoomy(): boolean {
  if (typeof window === 'undefined' || !window.matchMedia) return true
  return window.matchMedia('(min-width: 1280px)').matches
}
