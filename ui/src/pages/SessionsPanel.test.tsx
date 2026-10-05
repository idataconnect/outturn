import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router'
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
  listAgents: vi.fn(async () => [
    { id: 'a1', name: 'Helper', slug: 'helper', description: '', enabled: true, can_chat: true },
    { id: 'a2', name: 'Other', slug: 'other', description: '', enabled: true, can_chat: true },
  ]),
  listSessions: vi.fn(async () => [
    { id: 's1', agent_id: 'a1', title: 'First chat' },
    { id: 's2', agent_id: 'a1', title: 'Second chat' },
  ]),
  createSession: vi.fn(async () => ({ id: 's3', agent_id: 'a1', title: null })),
  renameSession: vi.fn(),
  sessionName: (s: { title?: string | null }) => s?.title ?? 'Untitled',
}))

const takeSeen: Array<(() => string) | undefined> = []
// What the runtime reports as having failed, and what dismissing it called.
let runtimeError: string | null = null
const dismissError = vi.fn()
vi.mock('../lib/useChatRuntime', () => ({
  useChatRuntime: (
    _id: string | null,
    _renamed?: (t: string) => void,
    take?: () => string,
  ) => {
    takeSeen.push(take)
    return { runtime: null, error: runtimeError, stopping: false, dismissError }
  },
}))

vi.mock('@assistant-ui/react', () => ({
  AssistantRuntimeProvider: ({ children }: { children: React.ReactNode }) => <>{children}</>,
}))

// Records what the page asked of it, so a test can tell a focus request was
// made without rendering assistant-ui. Thread.test covers what it does with one.
vi.mock('../components/Thread', () => ({
  default: ({
    focusRequest,
    takeAttachments,
  }: {
    focusRequest?: number
    takeAttachments?: { current: (() => string) | null }
  }) => {
    // The composer fills this; the page is supposed to hand it to the runtime.
    if (takeAttachments) takeAttachments.current = () => '[image: session/a.png]'
    return (
      <div data-testid="thread" data-focus-request={focusRequest}>
        thread
      </div>
    )
  },
}))
vi.mock('../components/SidePane', () => ({ default: () => <div>pane</div> }))
vi.mock('../components/PromptBanner', () => ({ default: () => null }))

const signedIn: SessionState = {
  status: 'authenticated',
  displayName: 'Tester',
  workspaces: [],
  session: {
    session_id: 's', workspace_id: 'w', display_name: 'Tester',
    roles: [], authorities: ['sessions:create'], workspaces: [],
  },
}

function show(at = '/sessions/s1') {
  return render(
    <MemoryRouter initialEntries={[at]}>
      <SessionContext.Provider value={signedIn}>
        <Routes>
          <Route path="/sessions" element={<Chat />} />
          <Route path="/sessions/new" element={<Chat draft />} />
          <Route path="/sessions/:sessionId" element={<Chat />} />
        </Routes>
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
    // In the list, as well as in the header of the one that is open.
    expect(within(document.querySelector('aside')!).getByText('First chat')).toBeInTheDocument()

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

  it('is asked for when an agent is chosen for a new chat', async () => {
    const user = userEvent.setup()
    setWidth(1440)
    show('/sessions/new')

    // Two agents, so the page asks rather than choosing for the reader.
    await user.click(await screen.findByRole('link', { name: 'Helper' }))

    const thread = await screen.findByTestId('thread')
    expect(Number(thread.dataset.focusRequest)).toBeGreaterThan(0)
  })
})

describe('starting a chat', () => {
  beforeEach(() => {
    localStorage.clear()
    vi.clearAllMocks()
  })

  it('creates nothing until something is sent', async () => {
    // Choosing an agent and thinking better of it used to leave an empty
    // session in everybody's history. The chat is made by the first send.
    const { createSession } = await import('../lib/chat')
    const user = userEvent.setup()
    setWidth(1440)
    show('/sessions/new')

    await user.click(await screen.findByRole('link', { name: 'Helper' }))
    await screen.findByTestId('thread')

    expect(createSession).not.toHaveBeenCalled()
  })
})

describe('what the composer attaches', () => {
  beforeEach(() => {
    localStorage.clear()
    vi.clearAllMocks()
    takeSeen.length = 0
  })

  it('reaches the runtime that sends the message', async () => {
    // The composer holds the attachments and the runtime does the sending, so
    // the page is the only place they can meet. Wiring the ref into the
    // component but forgetting to pass it to the runtime leaves a thumbnail
    // that never clears and a reference the model never sees -- which is
    // exactly what happened, and no test noticed.
    setWidth(1440)
    show()

    await screen.findByTestId('thread')
    const take = takeSeen.at(-1)
    expect(take, 'the page never gave the runtime a way to collect attachments').toBeTypeOf(
      'function',
    )
    expect(take?.()).toBe('[image: session/a.png]')
  })
})

describe('a new chat', () => {
  beforeEach(() => {
    localStorage.clear()
    vi.clearAllMocks()
  })

  it('does not ask who with when there is only one to have it with', async () => {
    const { listAgents } = await import('../lib/chat')
    vi.mocked(listAgents).mockResolvedValueOnce([
      { id: 'a1', name: 'Helper', slug: 'helper', description: '', enabled: true, can_chat: true },
    ])
    setWidth(1440)
    // Watched from the first render, since what is being tested is that the
    // picker is never drawn -- not merely that it is gone by the end.
    let asked = false
    const watch = new MutationObserver(() => {
      if (document.body.textContent?.match(/who would you like/i)) asked = true
    })
    watch.observe(document.body, { subtree: true, childList: true, characterData: true })
    show('/sessions/new')
    if (document.body.textContent?.match(/who would you like/i)) asked = true

    expect(await screen.findByText('New chat with Helper')).toBeInTheDocument()
    watch.disconnect()
    expect(asked).toBe(false)
  })

  it('drops a notice about a dead link once somebody starts a new chat', async () => {
    const user = userEvent.setup()
    setWidth(1440)
    show('/sessions/gone')

    expect(await screen.findByText(/not available in this workspace/i)).toBeInTheDocument()

    await user.click(screen.getByRole('link', { name: /new chat/i }))

    await screen.findByText(/who would you like/i)
    expect(screen.queryByText(/not available in this workspace/i)).toBeNull()
  })
})

describe('a failed turn', () => {
  beforeEach(() => {
    runtimeError = null
    dismissError.mockClear()
  })

  it('can be put away', async () => {
    const user = userEvent.setup()
    setWidth(1440)
    runtimeError = 'incomplete utf-8 byte sequence from index 7804'
    show()

    expect(await screen.findByRole('alert')).toHaveTextContent(/incomplete utf-8/)
    await user.click(screen.getByRole('button', { name: /dismiss/i }))
    expect(dismissError).toHaveBeenCalledOnce()
  })

  it('offers no dismiss for a link that went nowhere', async () => {
    setWidth(1440)
    show('/sessions/gone')

    expect(await screen.findByText(/not available in this workspace/i)).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /dismiss/i })).toBeNull()
  })
})
