import { useEffect, type ReactNode } from 'react'
import { useNavigate } from 'react-router'

import { configured } from '../lib/models'
import { paths } from '../lib/paths'
import { useSession } from '../lib/session'

/** Set once the operator has been taken to setup in this browser session. */
const TAKEN = 'outturn.setup-offered'

/** The same, for a browser that refuses storage: once a page load. */
let taken = false

/**
 * The landing page, except for an operator signing in to a deployment where
 * no chain answers assistant traffic: they are taken to setup, once a browser
 * session, as a push rather than a replace so Back returns here. Not taken
 * again in that session, so leaving setup is not undone by coming back.
 */
export default function FirstRun({ children }: { children: ReactNode }) {
  const state = useSession()
  const navigate = useNavigate()
  const operator = state.status === 'authenticated' && state.session.roles.includes('system_admin')

  useEffect(() => {
    if (!operator) return
    try {
      if (sessionStorage.getItem(TAKEN) !== null) taken = true
    } catch {
      // Storage refused: `taken` alone remembers.
    }
    if (taken) return
    let current = true
    configured()
      .then((config) => {
        if (!current || taken || config.assignments.assistant) return
        taken = true
        try {
          sessionStorage.setItem(TAKEN, '1')
        } catch {
          // As above.
        }
        navigate(paths.platformSetup)
      })
      // Nothing known, nothing done: the Models page says the same later.
      .catch(() => {})
    return () => {
      current = false
    }
  }, [operator, navigate])

  return <>{children}</>
}
