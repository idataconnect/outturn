import { api, ApiError } from './api'
import type { StoredFile } from './chat'

/**
 * Sending files into a conversation's storage, whatever their size.
 *
 * A file up to `SINGLE_LIMIT` goes in one request, as it always has -- that is
 * also where a HEIC photo is converted. Anything larger goes in pieces
 * (docs/large-uploads.md): each piece is its own request, retried on its own,
 * and an upload interrupted by a closed tab or a dropped connection carries on
 * from the pieces that arrived rather than starting over.
 */

/** The most one request carries, as the API bounds it. */
export const SINGLE_LIMIT = 25 * 1024 * 1024

/** Pieces of one file in flight at once. */
const PIECES_AT_ONCE = 3

/** Tries for each piece before the file is reported as failed. */
const TRIES = 4

type Scope = StoredFile['scope']

/** How far a file has got: bytes the server has, of how many. */
export interface Progress {
  sent: number
  size: number
  /** Pieces arrived and in all, for a file sent in pieces. */
  pieces?: { done: number; total: number }
}

interface Started {
  upload_id: string
  piece_size: number
  pieces: number
}

interface UploadState {
  received: number[]
  state: string
  piece_size: number
  pieces: number
}

/** A path under a scope, each segment encoded on its own so a folder's
 *  slashes stay slashes. */
export function encodePath(path: string): string {
  return path.split('/').map(encodeURIComponent).join('/')
}

const base = (sessionId: string) => `/v1/agent-sessions/${sessionId}/uploads`

/** Where an unfinished upload of this file is remembered, so a reload picks
 *  it up. Keyed by what identifies the file as the same one. */
function resumeKey(sessionId: string, scope: Scope, path: string, file: File): string {
  return `outturn:upload:${sessionId}:${scope}:${path}:${file.size}:${file.lastModified}`
}

function remembered(key: string): string | null {
  try {
    return localStorage.getItem(key)
  } catch {
    return null
  }
}

function remember(key: string, id: string | null) {
  try {
    if (id) localStorage.setItem(key, id)
    else localStorage.removeItem(key)
  } catch {
    // A private window: the upload still works, it just cannot resume.
  }
}

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms))

/** Whether a failure is worth trying again: the network, or the server
 *  having a moment. A refusal is not. */
function transient(e: unknown): boolean {
  if (!(e instanceof ApiError)) return true
  return e.status === 0 || e.status === 429 || e.status >= 500
}

/**
 * Sends `file` to `path` under `scope`, reporting progress as it goes.
 */
export async function sendFile(
  sessionId: string,
  scope: Scope,
  path: string,
  file: File,
  onProgress: (p: Progress) => void,
  signal?: AbortSignal,
): Promise<StoredFile> {
  if (file.size <= SINGLE_LIMIT) {
    onProgress({ sent: 0, size: file.size })
    const stored = await api<StoredFile>(
      `/v1/agent-sessions/${sessionId}/files/${scope}/${encodePath(path)}`,
      {
        method: 'PUT',
        headers: { 'content-type': file.type || 'application/octet-stream' },
        body: file,
        signal,
      },
    )
    onProgress({ sent: file.size, size: file.size })
    return stored
  }

  const key = resumeKey(sessionId, scope, path, file)
  let upload: Started | null = null
  let received = new Set<number>()

  // An upload this browser began for this same file, if it is still open.
  const known = remembered(key)
  if (known) {
    try {
      const state = await api<UploadState>(`${base(sessionId)}/${known}`, { signal })
      if (state.state === 'open') {
        upload = { upload_id: known, piece_size: state.piece_size, pieces: state.pieces }
        received = new Set(state.received)
      }
    } catch {
      // Gone, or never ours: start again.
    }
  }
  if (!upload) {
    upload = await api<Started>(base(sessionId), {
      method: 'POST',
      body: JSON.stringify({ scope, path, size: file.size }),
      signal,
    })
    remember(key, upload.upload_id)
  }
  const { upload_id, piece_size, pieces } = upload

  const pieceLen = (n: number) => (n < pieces ? piece_size : file.size - piece_size * (pieces - 1))
  const report = () => {
    let sent = 0
    for (const n of received) sent += pieceLen(n)
    onProgress({ sent, size: file.size, pieces: { done: received.size, total: pieces } })
  }
  report()

  const todo: number[] = []
  for (let n = 1; n <= pieces; n++) if (!received.has(n)) todo.push(n)

  async function sendPiece(n: number) {
    const start = (n - 1) * piece_size
    const blob = file.slice(start, start + pieceLen(n))
    for (let attempt = 1; ; attempt++) {
      try {
        await api<void>(`${base(sessionId)}/${upload_id}/pieces/${n}`, {
          method: 'PUT',
          headers: { 'content-type': 'application/octet-stream' },
          body: blob,
          signal,
        })
        received.add(n)
        report()
        return
      } catch (e) {
        if (signal?.aborted || attempt >= TRIES || !transient(e)) throw e
        await wait(500 * 2 ** attempt)
      }
    }
  }

  // A few pieces at a time, each taking the next one left when it is done.
  const workers = Array.from({ length: Math.min(PIECES_AT_ONCE, todo.length) }, async () => {
    for (let n = todo.shift(); n !== undefined; n = todo.shift()) {
      await sendPiece(n)
    }
  })
  await Promise.all(workers)

  const stored = await api<StoredFile>(`${base(sessionId)}/${upload_id}/complete`, {
    method: 'POST',
    signal,
  })
  remember(key, null)
  return stored
}
