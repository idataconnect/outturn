import { Aes256Gcm, CipherSuite, DhkemX25519HkdfSha256, HkdfSha256 } from '@hpke/core'

/**
 * Seals a credential to the gateway, in this browser, so the API never holds
 * it. HPKE base mode with the suite and label `src/egress/seal.rs` opens with;
 * the binding is the associated data, sent beside the seal as the exact bytes
 * that were sealed. See docs/sealed-credentials.md.
 *
 * What this cannot protect against is an API compromised while somebody is
 * typing into a page it serves -- that API serves this script. It protects
 * against everyone else: the database, its backups, and an API compromised
 * later.
 */
export const INFO = 'outturn seal v1 egress-credential'

const utf8 = new TextEncoder()

function fromHex(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2)
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16)
  return out
}

export function toBase64(bytes: Uint8Array): string {
  let s = ''
  for (const b of bytes) s += String.fromCharCode(b)
  return btoa(s)
}

export type Binding = {
  credential: string
  kind: 'static'
  workspaces: string[]
  hosts: string[]
  header: string
}

/** The binding's bytes, written once: what is sealed under is what is sent. */
export function bindingBytes(binding: Binding): Uint8Array {
  return utf8.encode(JSON.stringify(binding))
}

/** `enc || ciphertext`, as the gateway reads it. */
export async function seal(
  publicKeyHex: string,
  binding: Uint8Array,
  secret: string,
): Promise<Uint8Array> {
  const suite = new CipherSuite({
    kem: new DhkemX25519HkdfSha256(),
    kdf: new HkdfSha256(),
    aead: new Aes256Gcm(),
  })
  const recipientPublicKey = await suite.kem.deserializePublicKey(
    fromHex(publicKeyHex).buffer as ArrayBuffer,
  )
  const { ct, enc } = await suite.seal(
    { recipientPublicKey, info: utf8.encode(INFO).buffer as ArrayBuffer },
    utf8.encode(secret).buffer as ArrayBuffer,
    binding.buffer as ArrayBuffer,
  )
  const out = new Uint8Array(enc.byteLength + ct.byteLength)
  out.set(new Uint8Array(enc), 0)
  out.set(new Uint8Array(ct), enc.byteLength)
  return out
}
