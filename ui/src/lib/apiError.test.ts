import { describe, expect, it } from 'vitest'

import { ApiError } from './api'

describe('an error response', () => {
  it('gives its code and message from the API', async () => {
    const e = await ApiError.from(
      new Response(JSON.stringify({ code: 'conflict', message: 'that name is taken' }), {
        status: 409,
      }),
    )
    expect([e.status, e.code, e.message]).toEqual([409, 'conflict', 'that name is taken'])
  })

  it('is its text when it is not the API answering', async () => {
    const e = await ApiError.from(new Response('Bad Gateway from the proxy', { status: 502 }))
    expect([e.code, e.message]).toEqual([undefined, 'Bad Gateway from the proxy'])
  })
})
