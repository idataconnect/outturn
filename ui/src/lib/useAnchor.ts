import { useEffect, useState } from 'react'
import { useLocation } from 'react-router'

/**
 * The element the URL's anchor names, scrolled to once it exists, and its id
 * for as long as it should look picked out.
 *
 * The browser does this itself only for an element present when the page
 * loads, and a list read from the API is not: `ready` says when it has
 * arrived. The highlight fades, because an item still lit up a minute later
 * reads as something being wrong with it.
 */
export function useAnchor(ready: boolean): string | null {
  const { hash } = useLocation()
  const [target, setTarget] = useState<string | null>(null)

  useEffect(() => {
    const id = decodeURIComponent(hash.replace(/^#/, ''))
    if (!ready || !id) return
    const element = document.getElementById(id)
    if (!element) return
    element.scrollIntoView({ block: 'center', behavior: 'smooth' })
    setTarget(id)
    const timer = setTimeout(() => setTarget(null), 2500)
    return () => clearTimeout(timer)
  }, [hash, ready])

  return target
}

/** Where a schedule or webhook is managed: its agent's page, at its own row. */
export const triggerHref = (agentId: string, kind: 'schedule' | 'webhook', id: string | null) =>
  `/agents/${agentId}/edit${id ? `#${kind}-${id}` : ''}`
