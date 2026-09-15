import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi, beforeEach } from 'vitest'

import FilePreview from './FilePreview'

const fetchMock = vi.fn()

/** A preview response, as the server would send one. */
function served(body: string, type: string, truncated = false) {
  return {
    ok: true,
    status: 200,
    headers: new Headers({ 'content-type': type, 'x-outturn-truncated': String(truncated) }),
    text: async () => body,
    blob: async () => new Blob([body], { type }),
  }
}

beforeEach(() => {
  fetchMock.mockReset()
  vi.stubGlobal('fetch', fetchMock)
  vi.stubGlobal('URL', {
    ...URL,
    createObjectURL: vi.fn(() => 'blob:preview'),
    revokeObjectURL: vi.fn(),
  })
})

const show = (path: string, onClose = () => {}) =>
  render(<FilePreview sessionId="s1" path={path} onClose={onClose} />)

describe('looking at a file', () => {
  it('shows text as text', async () => {
    fetchMock.mockResolvedValue(served('hello from the log', 'text/plain; charset=utf-8'))
    show('session/app.log')
    expect(await screen.findByText('hello from the log')).toBeInTheDocument()
  })

  it('renders markdown as markdown', async () => {
    // Same bytes either way; what differs is whether a heading is a heading.
    fetchMock.mockResolvedValue(served('# Title', 'text/plain; charset=utf-8'))
    show('session/notes.md')
    expect(await screen.findByRole('heading', { name: 'Title' })).toBeInTheDocument()
  })

  it('leaves a hash alone in a file that is not markdown', async () => {
    fetchMock.mockResolvedValue(served('# not a heading', 'text/plain; charset=utf-8'))
    show('session/notes.txt')
    expect(await screen.findByText('# not a heading')).toBeInTheDocument()
    expect(screen.queryByRole('heading')).not.toBeInTheDocument()
  })

  it('shows an image', async () => {
    fetchMock.mockResolvedValue(served('bytes', 'image/png'))
    show('session/shot.png')
    const image = await screen.findByRole('img')
    expect(image).toHaveAttribute('src', 'blob:preview')
  })

  it('offers a download for what it cannot show', async () => {
    // The server refuses anything outside its allowlist, and a modal that
    // says so with a way out is better than a row that does nothing.
    fetchMock.mockResolvedValue({ ok: false, status: 415, headers: new Headers(), text: async () => '' })
    show('session/archive.zip')
    expect(await screen.findByText(/cannot be previewed/i)).toBeInTheDocument()
    expect(screen.getByRole('link', { name: /download it instead/i })).toBeInTheDocument()
  })

  it('says when it is showing only the start of a long file', async () => {
    fetchMock.mockResolvedValue(served('a lot of text', 'text/plain; charset=utf-8', true))
    show('session/huge.log')
    expect(await screen.findByText(/first 256KB/i)).toBeInTheDocument()
  })
})

describe('getting out of the preview', () => {
  it('closes on Escape', async () => {
    // A dialog that cannot be dismissed from the keyboard is one somebody is
    // trapped in.
    const onClose = vi.fn()
    fetchMock.mockResolvedValue(served('text', 'text/plain; charset=utf-8'))
    show('session/a.txt', onClose)
    await screen.findByText('text')

    await userEvent.keyboard('{Escape}')
    expect(onClose).toHaveBeenCalled()
  })

  it('closes on a click outside, but not on one inside', async () => {
    const onClose = vi.fn()
    fetchMock.mockResolvedValue(served('text', 'text/plain; charset=utf-8'))
    show('session/a.txt', onClose)
    const body = await screen.findByText('text')

    // Reading the file must not dismiss what is being read.
    await userEvent.click(body)
    expect(onClose).not.toHaveBeenCalled()

    await userEvent.click(screen.getByRole('dialog').parentElement!)
    expect(onClose).toHaveBeenCalled()
  })

  it('is announced as a dialog and puts focus inside it', async () => {
    fetchMock.mockResolvedValue(served('text', 'text/plain; charset=utf-8'))
    show('session/a.txt')

    const dialog = await screen.findByRole('dialog')
    expect(dialog).toHaveAttribute('aria-modal', 'true')
    await waitFor(() =>
      expect(screen.getByRole('button', { name: /close preview/i })).toHaveFocus(),
    )
  })
})
