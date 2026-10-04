import { describe, expect, it } from 'vitest'

import { bindingBytes, seal } from './seal'

describe('sealing in the browser', () => {
  /// The layout the gateway reads: a 32-byte encapsulated key, then the
  /// ciphertext with its 16-byte tag. That the gateway actually opens one is
  /// pinned in src/egress/seal.rs, against a seal this file made.
  it('is the encapsulated key followed by the ciphertext', async () => {
    const binding = bindingBytes({
      credential: '00000000-0000-0000-0000-000000000000',
      kind: 'static',
      workspaces: ['01920000-0000-7000-8000-00000000000a'],
      hosts: ['books.example.com'],
      header: 'authorization',
    })
    const sealed = await seal(
      '13be4feaeaf204c7fd3358fc9c00721881d174278128227ec674f37f7fe97b6d',
      binding,
      'secret',
    )
    expect(sealed.length).toBe(32 + 'secret'.length + 16)
  })
})
