import assert from 'node:assert/strict'
import test from 'node:test'
import { createUuid } from './Uuid'

test('UUID generation uses the platform implementation when available', () => {
  const expected = '00000000-0000-4000-8000-000000000001'
  const cryptoApi = { randomUUID: () => expected } as Crypto

  assert.equal(createUuid(cryptoApi), expected)
})

test('UUID generation falls back to secure random bytes outside secure contexts', () => {
  const cryptoApi = {
    getRandomValues: (bytes: Uint8Array) => {
      bytes.fill(0xff)
      return bytes
    },
  } as unknown as Crypto

  assert.equal(createUuid(cryptoApi), 'ffffffff-ffff-4fff-bfff-ffffffffffff')
})

test('UUID generation fails closed without secure randomness', () => {
  assert.throws(() => createUuid(null as unknown as Crypto), /Secure random values are unavailable/)
})
