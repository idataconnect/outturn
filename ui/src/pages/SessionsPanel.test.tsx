import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import Chat from './Chat'
import { SessionContext, type SessionState } from '../lib/session'

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

vi.mock('../lib/chat', () => ({
  listAgents: vi.fn(async () => [{ id: 'a1', name: 'Helper' }]),
  listSessions: vi.fn(async () => [
    { id: 's1', agent_id: 'a1', title: 'First chat' },
    { id: 's2', agent_id: 'a1', title: 'Second chat' },
  ]),
  createSession: vi.fn(async () => ({ id: 's3', agent_id: 'a1', title: null })),
  renameSession: vi.fn(),
  sessionName: (s: { title?: string | null }) => s?.title ?? 'Untitled',
}))

vi.mock('../lib/useChatRuntime', () => ({
  useChatRuntime: () => ({ runtime: null, error: null, stopping: false }),
}))

vi.mock('@assistant-ui/react', () => ({
  AssistantRuntimeProvider: ({ children }: { children: React.ReactNode }) => <>{children}</>,
}))

// Records what the page asked of it, so a test can tell a focus request was
// made without rendering assistant-ui. Thread.test covers what it does with one.
vi.mock('../components/Thread', () => ({
  default: ({ focusRequest }: { focusRequest?: number }) => (
    <div data-testid="thread" data-focus-request={focusRequest}>thread</div>
  ),
}))
vi.mock('../components/SidePane', () => ({ default: () => <div>pane</div> }))

const signedIn: SessionState = {
  status: 'authenticated',
  displayName: 'Tester',
  workspaces: [],
  session: {
    session_id: 's', workspace_id: 'w', display_name: 'Tester',
    roles: [], authorities: ['sessions:create'], workspaces: [],
  },
}

function show() {
  return render(
    <MemoryRouter initialEntries={['/sessions/s1']}>
      <SessionContext.Provider value={signedIn}>
        <Chat />
      </SessionContext.Provider>
    </MemoryRouter>,
  )
}

describe('picking a session', () => {
  beforeEach(() => {
    localStorage.clear()
    vi.clearAllMocks()
  })

  it('leaves the list open where it sits beside the thread', async () => {
    const user = userEvent.setup()
    setWidth(1440)
    show()

    const second = await screen.findByText('Second chat')
    expect(screen.getByText('First chat')).toBeInTheDocument()

    await user.click(second)

    // The list is inline here; closing it would take the sidebar away every
    // time somebody used it.
    await waitFor(() =>
      expect(document.querySelector('aside')?.className).not.toContain('hidden'),
    )
  })

  it('dismisses the list on a phone, where it covers the conversation', async () => {
    const user = userEvent.setup()
    setWidth(390)
    show()

    // Opened by hand, since a phone starts with it shut.
    await user.click(await screen.findByRole('button', { name: /show sessions/i }))
    const second = await screen.findByText('Second chat')

    await user.click(second)

    // `hidden` is a class, and jsdom applies no CSS, so the text stays in the
    // DOM either way -- what changed is whether the panel is shown at all.
    await waitFor(() =>
      expect(document.querySelector('aside')?.className).toContain('hidden'),
    )
  })
})

describe('asking for the cursor', () => {
  beforeEach(() => {
    localStorage.clear()
    vi.clearAllMocks()
  })

  it('is asked for when a session is picked', async () => {
    const user = userEvent.setup()
    setWidth(1440)
    show()

    const thread = await screen.findByTestId('thread')
    const before = thread.dataset.focusRequest

    await user.click(await screen.findByText('Second chat'))

    await waitFor(() =>
      expect(screen.getByTestId('thread').dataset.focusRequest).not.toBe(before),
    )
  })

  it('is asked for when a session is started', async () => {
    const user = userEvent.setup()
    setWidth(1440)
    show()

    const thread = await screen.findByTestId('thread')
    const before = thread.dataset.focusRequest

    await user.click(await screen.findByRole('button', { name: 'Helper' }))

    await waitFor(() =>
      expect(screen.getByTestId('thread').dataset.focusRequest).not.toBe(before),
    )
  })
})
