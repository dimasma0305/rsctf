import assert from 'node:assert/strict'
import test from 'node:test'
import { collectFingerprintIdentityWith, FingerprintIdentityDependencies } from './FingerprintIdentityCore'
import { FingerprintCollectionError } from './FingerprintProbe'

const options = {
  enabled: true,
  apiPublicKey: null,
  translate: (key: string) => key,
}

const dependencies = (calls: string[]): FingerprintIdentityDependencies => ({
  requestChallenge: async (signal) => {
    calls.push(`challenge:${signal?.aborted ?? false}`)
    return { nonce: 'nonce', requiredSignals: ['lie_count'] }
  },
  collectPayload: async (challenge, signal) => {
    calls.push(`payload:${challenge.nonce}:${signal?.aborted ?? false}`)
    return { fingerprint: 'fingerprint', proof: 'proof' }
  },
  encrypt: async (value) => {
    calls.push(`encrypt:${value}`)
    return `encrypted:${value}`
  },
})

test('disabled fingerprint collection performs no telemetry work', async () => {
  const calls: string[] = []
  const result = await collectFingerprintIdentityWith({ ...options, enabled: false }, dependencies(calls))
  assert.deepEqual(result, {})
  assert.deepEqual(calls, [])
})

test('the shared identity path performs one challenge, one collection, and two encryptions', async () => {
  const calls: string[] = []
  const controller = new AbortController()
  const result = await collectFingerprintIdentityWith({ ...options, signal: controller.signal }, dependencies(calls))
  assert.deepEqual(result, {
    fingerprint: 'encrypted:fingerprint',
    fingerprintProof: 'encrypted:proof',
  })
  assert.deepEqual(calls, ['challenge:false', 'payload:nonce:false', 'encrypt:fingerprint', 'encrypt:proof'])
})

test('a failed fingerprint attempt does not retry until the caller starts a new operation', async () => {
  let attempts = 0
  const deps = dependencies([])
  deps.collectPayload = async () => {
    attempts += 1
    if (attempts === 1) throw new FingerprintCollectionError('required-signal-unavailable', ['lie_count'])
    return { fingerprint: 'fingerprint', proof: 'proof' }
  }

  await assert.rejects(
    collectFingerprintIdentityWith(options, deps),
    (error: unknown) => error instanceof FingerprintCollectionError && error.retriable
  )
  assert.equal(attempts, 1)
  await collectFingerprintIdentityWith(options, deps)
  assert.equal(attempts, 2)
})

test('aborting the shared identity path prevents encryption and submission evidence', async () => {
  const controller = new AbortController()
  let encrypted = 0
  const deps = dependencies([])
  deps.requestChallenge = async () => {
    controller.abort()
    return { nonce: 'late', requiredSignals: [] }
  }
  deps.encrypt = async (value) => {
    encrypted += 1
    return value
  }

  await assert.rejects(
    collectFingerprintIdentityWith({ ...options, signal: controller.signal }, deps),
    (error: unknown) => error instanceof Error && error.name === 'AbortError'
  )
  assert.equal(encrypted, 0)
})
