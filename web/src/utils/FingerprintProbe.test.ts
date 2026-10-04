import assert from 'node:assert/strict'
import test from 'node:test'
import {
  FingerprintCollectionError,
  assertRequiredFingerprintSignalsAvailable,
  settleFingerprintProbe,
} from './FingerprintProbe'

test('optional browser probes settle independently after rejection or timeout', async () => {
  const failed = new Set<string>()
  const good = await settleFingerprintProbe('good', async () => 'value', failed, { probeTimeoutMs: 25 })
  const rejected = await settleFingerprintProbe('rejected', async () => {
    throw new Error('unsupported')
  }, failed, { probeTimeoutMs: 25 })
  const started = Date.now()
  const timedOut = await settleFingerprintProbe('hung', () => new Promise<never>(() => {}), failed, {
    probeTimeoutMs: 25,
  })

  assert.equal(good, 'value')
  assert.equal(rejected, undefined)
  assert.equal(timedOut, undefined)
  assert.deepEqual([...failed].sort(), ['hung', 'rejected'])
  assert.ok(Date.now() - started < 500, 'a hung optional probe must settle promptly')
})

test('collection cancellation rejects instead of becoming optional-probe failure', async () => {
  const controller = new AbortController()
  const failed = new Set<string>()
  const pending = settleFingerprintProbe('hung', () => new Promise<never>(() => {}), failed, {
    signal: controller.signal,
    probeTimeoutMs: 5_000,
  })
  controller.abort()

  await assert.rejects(pending, (error: unknown) =>
    error instanceof FingerprintCollectionError && error.code === 'aborted')
  assert.equal(failed.size, 0)
})

test('a failed or unknown required signal fails visibly', () => {
  assert.doesNotThrow(() => assertRequiredFingerprintSignalsAvailable(['lie_count'], new Set()))
  assert.throws(
    () => assertRequiredFingerprintSignalsAvailable(['lie_count'], new Set(['lies'])),
    (error: unknown) => error instanceof FingerprintCollectionError && error.code === 'required-signal-unavailable'
  )
  assert.throws(
    () => assertRequiredFingerprintSignalsAvailable(['future_signal'], new Set()),
    (error: unknown) => error instanceof FingerprintCollectionError && error.code === 'required-signal-unavailable'
  )
})
