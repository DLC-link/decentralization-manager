// Mock wallet keys. A real owner keeps the private key in their own wallet.
// The mock keeps it in this browser's storage so that one person can act as
// every owner.

export type KeyPair = { publicKey: string; privateKeyJwk: JsonWebKey }

export function toHex(bytes: Uint8Array): string {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')
}

export function fromHex(hex: string): Uint8Array<ArrayBuffer> {
  const bytes = new Uint8Array(hex.length / 2)
  for (let i = 0; i < bytes.length; i++) {
    bytes[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16)
  }
  return bytes
}

export async function generateKeyPair(): Promise<KeyPair> {
  const keys = (await crypto.subtle.generateKey({ name: 'Ed25519' }, true, [
    'sign',
    'verify',
  ])) as CryptoKeyPair
  const raw = new Uint8Array(await crypto.subtle.exportKey('raw', keys.publicKey))
  return {
    publicKey: toHex(raw),
    privateKeyJwk: await crypto.subtle.exportKey('jwk', keys.privateKey),
  }
}

/** Signs the bytes that `digestHex` encodes and returns the signature as hex. */
export async function signDigest(privateKeyJwk: JsonWebKey, digestHex: string): Promise<string> {
  const key = await crypto.subtle.importKey('jwk', privateKeyJwk, { name: 'Ed25519' }, false, [
    'sign',
  ])
  const signature = await crypto.subtle.sign({ name: 'Ed25519' }, key, fromHex(digestHex))
  return toHex(new Uint8Array(signature))
}
