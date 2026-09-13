import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import App from '../App'

function setWidth(px: number) {
  vi.stubGlobal('matchMedia', (query: string) => {
    const min = Number(/min-width:\s*(\d+)px/.exec(query)?.[1] ?? 0)
    return {
      matches: px >= min,
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
    }
  })
}

// The shell only draws its nav for someone signed in.
vi.mock('../lib/api', () => ({
  ApiError: class extends Error {},
  api: vi.fn(async (path: string) => {
    if (path === '/v1/session') {
      return {
        session_id: 's', workspace_id: 'w', display_name: 'Tester',
        roles: [], authorities: [], workspaces: [],
      }
    }
    return {}
  }),
}))

describe('nav collapse control', () => {
  beforeEach(() => {
    localStorage.clear()
    setWidth(1440)
  })

  it('collapses from the header and can be expanded again', async () => {
    const user = userEvent.setup()
    render(<App />)

    const collapse = await screen.findByRole('button', { name: 'Collapse navigation' })
    await user.click(collapse)

    // Railed: the collapse control is gone, but a way back must remain.
    expect(screen.queryByRole('button', { name: 'Collapse navigation' })).not.toBeInTheDocument()
    const expand = screen.getByRole('button', { name: 'Expand navigation' })
    await user.click(expand)

    expect(await screen.findByRole('button', { name: 'Collapse navigation' })).toBeInTheDocument()
  })
})
