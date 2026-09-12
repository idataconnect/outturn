import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import FilesPanel from './FilesPanel'
import { SessionContext, type SessionState } from '../lib/session'

// The panel talks to the API for everything it shows; the tests are about
// what a drop does, not about how a file is transferred.
vi.mock('../lib/chat', () => ({
  listFiles: vi.fn(async () => []),
  uploadFile: vi.fn(async () => ({ path: 'session/notes.txt', size: 4, scope: 'session' })),
  deleteFile: vi.fn(async () => undefined),
  fileUrl: (sessionId: string, path: string) => `/v1/sessions/${sessionId}/files/${path}`,
}))

import { listFiles, uploadFile } from '../lib/chat'

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
    vi.mocked(listFiles).mockResolvedValue([])
  })

  it('uploads a dropped file to the selected scope', async () => {
    const user = userEvent.setup()
    show()
    await waitFor(() => expect(listFiles).toHaveBeenCalled())

    const file = new File(['hi'], 'notes.txt', { type: 'text/plain' })
    await user.upload(screen.getByTestId('files-input'), file)

    await waitFor(() => expect(uploadFile).toHaveBeenCalledWith('abc', 'session', file))
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
