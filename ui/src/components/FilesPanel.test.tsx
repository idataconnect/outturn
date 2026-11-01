import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import FilesPanel from './FilesPanel'
import { SessionContext, type SessionState } from '../lib/session'

// The panel talks to the API for everything it shows; the tests are about
// what a drop does, not about how a file is transferred.
vi.mock('../lib/chat', () => ({
  listFiles: vi.fn(async () => ({ items: [], more: {} })),
  deleteFile: vi.fn(async () => undefined),
  fileUrl: (sessionId: string, path: string) => `/v1/sessions/${sessionId}/files/${path}`,
}))

vi.mock('../lib/uploads', () => ({
  sendFile: vi.fn(async () => ({ path: 'session/notes.txt', size: 4, scope: 'session' })),
}))

import { listFiles } from '../lib/chat'
import { sendFile } from '../lib/uploads'

function signedIn(authorities: string[]): SessionState {
  return {
    status: 'authenticated',
    displayName: 'Tester',
    workspaces: [],
    session: {
      session_id: 's',
      workspace_id: 'w',
      display_name: 'Tester',
      roles: [],
      authorities,
      workspaces: [],
    },
  }
}

function show(authorities = ['sessions:create', 'storage:workspace:write']) {
  return render(
    <SessionContext.Provider value={signedIn(authorities)}>
      <FilesPanel sessionId="abc" />
    </SessionContext.Provider>,
  )
}

/** A drag carrying `files`, shaped enough for the handlers that read it. */
function dragWith(...files: File[]) {
  return {
    dataTransfer: {
      files,
      items: files.map((f) => ({ kind: 'file', type: f.type, getAsFile: () => f })),
      types: ['Files'],
    },
  }
}

const panel = () => screen.getByRole('complementary')

// `fireEvent` rather than `user-event`: jsdom builds no DataTransfer, so the
// drag payload has to be supplied by hand.
function fireEnter(el: Element, drag: ReturnType<typeof dragWith>) {
  fireEvent.dragEnter(el, drag)
  fireEvent.dragOver(el, drag)
}

function fireLeave(el: Element) {
  fireEvent.dragLeave(el)
}

describe('FilesPanel drop target', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(listFiles).mockResolvedValue({ items: [], more: {} })
  })

  it('uploads a dropped file to the selected scope', async () => {
    const user = userEvent.setup()
    show()
    await waitFor(() => expect(listFiles).toHaveBeenCalled())

    const file = new File(['hi'], 'notes.txt', { type: 'text/plain' })
    await user.upload(screen.getByTestId('files-input'), file)

    await waitFor(() =>
      expect(sendFile).toHaveBeenCalledWith('abc', 'session', 'notes.txt', file, expect.any(Function)),
    )
    // Listed as uploaded once it is.
    expect(await screen.findByText(/Uploaded/)).toBeTruthy()
  })

  it('names a file from a picked folder by where it sat, and sends each once', async () => {
    const user = userEvent.setup()
    show()
    await waitFor(() => expect(listFiles).toHaveBeenCalled())

    const files = ['a.txt', 'b.txt', 'c.txt', 'd.txt'].map((n) => {
      const f = new File(['x'], n)
      Object.defineProperty(f, 'webkitRelativePath', { value: `policies/${n}` })
      return f
    })
    await user.upload(screen.getByTestId('folder-input'), files)

    await waitFor(() => expect(sendFile).toHaveBeenCalledTimes(4))
    expect(vi.mocked(sendFile).mock.calls.map((c) => c[2]).sort()).toEqual([
      'policies/a.txt',
      'policies/b.txt',
      'policies/c.txt',
      'policies/d.txt',
    ])
    await waitFor(() => expect(screen.getByText('4 of 4 uploaded')).toBeTruthy())
  })

  it('does not send again a file already here at the same size, until asked', async () => {
    vi.mocked(listFiles).mockResolvedValue({
      items: [{ path: 'session/notes.txt', size: 2, scope: 'session' }],
      more: {},
    })
    const user = userEvent.setup()
    show()
    await waitFor(() => expect(screen.getByText('session/notes.txt')).toBeTruthy())

    const file = new File(['hi'], 'notes.txt')
    await user.upload(screen.getByTestId('files-input'), file)
    expect(await screen.findByText('Already here, same size')).toBeTruthy()
    expect(sendFile).not.toHaveBeenCalled()

    await user.click(screen.getByRole('button', { name: 'Upload notes.txt again' }))
    await waitFor(() => expect(sendFile).toHaveBeenCalledTimes(1))
  })

  it('keeps a failed upload in the list with a way to try it again', async () => {
    const { ApiError } = await import('../lib/api')
    vi.mocked(sendFile).mockRejectedValueOnce(new ApiError(503, 'object storage is unavailable'))
    const user = userEvent.setup()
    show()
    await waitFor(() => expect(listFiles).toHaveBeenCalled())

    await user.upload(screen.getByTestId('files-input'), new File(['hi'], 'notes.txt'))
    expect(await screen.findByText('object storage is unavailable')).toBeTruthy()
    await user.click(screen.getByRole('button', { name: 'Retry notes.txt' }))
    expect(await screen.findByText(/Uploaded/)).toBeTruthy()
    expect(sendFile).toHaveBeenCalledTimes(2)
  })

  it('keeps the overlay up while the pointer crosses a child', async () => {
    show()
    await waitFor(() => expect(listFiles).toHaveBeenCalled())

    const file = new File(['hi'], 'notes.txt', { type: 'text/plain' })
    const drag = dragWith(file)

    // Enter the panel, then enter a child inside it. The child's `dragenter`
    // is paired with a `dragleave` on the panel, which is exactly the
    // sequence a single boolean gets wrong.
    fireEnter(panel(), drag)
    expect(screen.getByText(/drop to upload/i)).toBeInTheDocument()

    fireEnter(panel(), drag)
    fireLeave(panel())
    expect(screen.getByText(/drop to upload/i)).toBeInTheDocument()

    // Leaving for real takes it away.
    fireLeave(panel())
    expect(screen.queryByText(/drop to upload/i)).not.toBeInTheDocument()
  })

  it('names the scope it would upload to', async () => {
    show()
    await waitFor(() => expect(listFiles).toHaveBeenCalled())

    fireEnter(panel(), dragWith(new File(['hi'], 'notes.txt')))
    expect(screen.getByText(/drop to upload to this conversation/i)).toBeInTheDocument()
  })

  it('offers nothing to drop onto without a writable scope', async () => {
    show([])
    await waitFor(() => expect(listFiles).toHaveBeenCalled())

    fireEnter(panel(), dragWith(new File(['hi'], 'notes.txt')))
    expect(screen.queryByText(/drop to upload/i)).not.toBeInTheDocument()
  })
})

describe('FilesPanel paging', () => {
  /// The workspace scope grows with every conversation in it, so it is read a
  /// page at a time and the rest is asked for.
  it('reads the rest of a scope when asked', async () => {
    vi.clearAllMocks()
    vi.mocked(listFiles)
      .mockResolvedValueOnce({
        items: [{ path: 'workspace/a.csv', scope: 'workspace', size: 1 }],
        more: { workspace: 'a.csv' },
      })
      .mockResolvedValueOnce({
        items: [{ path: 'workspace/b.csv', scope: 'workspace', size: 1 }],
        more: {},
      })
    const user = userEvent.setup()
    show()
    await user.click(await screen.findByRole('button', { name: /More files in the workspace/ }))
    expect(listFiles).toHaveBeenLastCalledWith('abc', 'workspace', 'a.csv')
    expect(await screen.findByText('workspace/b.csv')).toBeInTheDocument()
    expect(screen.getByText('workspace/a.csv')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /More files/ })).not.toBeInTheDocument()
  })
})
