import { describe, expect, it, vi, beforeEach } from 'vitest'

import { uploadPastedImage } from './chat'

/** What `api` ends up calling. */
const fetchMock = vi.fn()

beforeEach(() => {
  fetchMock.mockReset()
  fetchMock.mockResolvedValue({
    ok: true,
    status: 201,
    headers: new Headers({ 'content-type': 'application/json' }),
    json: async () => ({ path: 'session/pasted.png', scope: 'session', size: 12 }),
    text: async () => '',
  })
  vi.stubGlobal('fetch', fetchMock)
})

const urlOf = () => String(fetchMock.mock.calls[0][0])
const initOf = () => fetchMock.mock.calls[0][1] as RequestInit

describe('storing an image off the clipboard', () => {
  it('invents a name, because a pasted blob has none', async () => {
    // A File off a disk has a name; a Blob off the clipboard has bytes and a
    // type and nothing else, so the old upload path had nothing to build a
    // path from.
    await uploadPastedImage('s1', new Blob(['x'], { type: 'image/png' }))
    expect(urlOf()).toMatch(/session\/pasted-\d{8}-\d{6}\.png$/)
  })

  it('keeps two pastes apart', async () => {
    // Both called `image.png` would mean the second quietly replacing the
    // first, which is a paste the person thought they had kept.
    vi.setSystemTime(new Date('2026-09-15T08:15:30Z'))
    await uploadPastedImage('s1', new Blob(['x'], { type: 'image/png' }))
    const first = urlOf()
    fetchMock.mockClear()
    vi.setSystemTime(new Date('2026-09-15T08:15:31Z'))
    await uploadPastedImage('s1', new Blob(['y'], { type: 'image/png' }))
    expect(urlOf()).not.toBe(first)
  })

  it('names the file by what the clipboard says it is', async () => {
    await uploadPastedImage('s1', new Blob(['x'], { type: 'image/jpeg' }))
    expect(urlOf()).toMatch(/\.jpg$/)
    // Carried as the blob's own type: `api` defaults a body to JSON, and an
    // image sent as application/json is one the server stores wrongly.
    expect(new Headers(initOf().headers).get('content-type')).toBe('image/jpeg')
  })

  it('stores something unexpected rather than refusing it', async () => {
    // Safari has pasted image/tiff. The host reads the bytes to decide what
    // an image is, so a wrong guess here costs nothing -- and refusing would
    // lose something somebody meant to keep.
    await uploadPastedImage('s1', new Blob(['x'], { type: 'image/tiff' }))
    expect(urlOf()).toMatch(/\.tiff$/)
  })

  it('sends the bytes as the body rather than as JSON', async () => {
    const blob = new Blob(['bytes'], { type: 'image/png' })
    await uploadPastedImage('s1', blob)
    expect(initOf().body).toBe(blob)
    expect(initOf().method).toBe('PUT')
  })

  it('puts it in the session scope, where the agent looks', async () => {
    await uploadPastedImage('s1', new Blob(['x'], { type: 'image/png' }))
    expect(urlOf()).toContain('/files/session/')
  })
})
