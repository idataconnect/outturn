import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import PromptBanner from './PromptBanner'
import { compactSession, getPromptStatus, type PromptStatus } from '../lib/chat'

vi.mock('../lib/chat', () => ({
  getPromptStatus: vi.fn(),
  compactSession: vi.fn(),
}))

const status = vi.mocked(getPromptStatus)
const compact = vi.mocked(compactSession)

const stale: PromptStatus = {
  composed_at: '2026-01-01T00:00:00Z',
  current: false,
  skills: [{ name: 'Bigcapital', kept: 2, live: 3 }],
}

const show = (props: Partial<Parameters<typeof PromptBanner>[0]> = {}) =>
  render(
    <MemoryRouter>
      <PromptBanner
        sessionId="s1"
        agentId="a1"
        compacting={false}
        compactions={0}
        {...props}
      />
    </MemoryRouter>,
  )

describe('PromptBanner', () => {
  beforeEach(() => {
    status.mockReset()
    compact.mockReset()
  })

  it('names the newer version and offers to compact or start fresh', async () => {
    status.mockResolvedValue(stale)
    show()
    expect(await screen.findByText(/Bigcapital v3 is published/i)).toBeTruthy()
    expect(screen.getByRole('button', { name: /compact this session/i })).toBeTruthy()
    expect(screen.getByRole('link', { name: /start a new one/i })).toBeTruthy()
  })

  it('shows progress the instant the button is pressed, before the request resolves', async () => {
    status.mockResolvedValue(stale)
    // Never resolves: the indicator must not wait on it, nor on any event.
    compact.mockReturnValue(new Promise(() => {}))
    show()
    const button = await screen.findByRole('button', { name: /compact this session/i })
    await userEvent.click(button)
    expect(screen.getByText(/Compacting this conversation/i)).toBeTruthy()
  })

  it('clears the progress once a compaction lands', async () => {
    status.mockResolvedValue(stale)
    compact.mockReturnValue(new Promise(() => {}))
    const { rerender } = show()
    await userEvent.click(await screen.findByRole('button', { name: /compact this session/i }))
    expect(screen.getByText(/Compacting this conversation/i)).toBeTruthy()

    // The compaction landed: the count rises and the status is now current,
    // so the banner clears its progress and then hides entirely.
    status.mockResolvedValue({ ...stale, current: true, skills: [] })
    rerender(
      <MemoryRouter>
        <PromptBanner sessionId="s1" agentId="a1" compacting={false} compactions={1} />
      </MemoryRouter>,
    )
    await vi.waitFor(() => {
      expect(screen.queryByText(/Compacting this conversation/i)).toBeNull()
      expect(screen.queryByText(/is published/i)).toBeNull()
    })
  })

  it('surfaces a failed request instead of hiding it', async () => {
    status.mockResolvedValue(stale)
    const { ApiError } = await import('../lib/api')
    compact.mockRejectedValue(new ApiError(429, 'too many requests'))
    show()
    await userEvent.click(await screen.findByRole('button', { name: /compact this session/i }))
    expect(await screen.findByText(/Compacting did not start: too many requests/i)).toBeTruthy()
  })

  it('shows progress for an automatic compaction it did not start', async () => {
    status.mockResolvedValue(stale)
    show({ compacting: true })
    expect(await screen.findByText(/Compacting this conversation/i)).toBeTruthy()
  })
})
