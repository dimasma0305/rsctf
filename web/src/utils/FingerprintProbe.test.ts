import assert from 'node:assert/strict'
import test from 'node:test'
import {
  collectRequiredFingerprintSignals,
  FINGERPRINT_PROBE_NAMES,
  FingerprintCollectionError,
  isAbortError,
  runFingerprintProbe,
} from './FingerprintProbe'

const validEvidence = () => ({
  lies: { totalLies: 3 },
  headless: { headlessRating: 17 },
  navigator: { platform: 'Linux x86_64', system: 'Linux' },
  workerScope: { platform: 'Linux x86_64', system: 'Linux', webglRenderer: 'Mesa Intel' },
  canvasWebgl: { parameters: { UNMASKED_RENDERER_WEBGL: 'ANGLE (Mesa Intel)' } },
})

test('every browser probe isolates rejection, unsupported APIs, and hangs', async () => {
  const failures = await Promise.all(
    FINGERPRINT_PROBE_NAMES.map((probe) =>
      runFingerprintProbe(probe, () => Promise.reject(new Error(`${probe} failed`)), { timeoutMs: 50 })
    )
  )
  assert.deepEqual(
    failures.map((result) => result.status === 'unavailable' && result.reason),
    FINGERPRINT_PROBE_NAMES.map(() => 'failed')
  )

  const unsupported = await Promise.all(
    FINGERPRINT_PROBE_NAMES.map((probe) => runFingerprintProbe(probe, () => undefined, { timeoutMs: 50 }))
  )
  assert.deepEqual(
    unsupported.map((result) => result.status === 'unavailable' && result.reason),
    FINGERPRINT_PROBE_NAMES.map(() => 'unsupported')
  )

  const hangs = await Promise.all(
    FINGERPRINT_PROBE_NAMES.map((probe) =>
      runFingerprintProbe(probe, () => new Promise<never>(() => undefined), { timeoutMs: 2 })
    )
  )
  assert.deepEqual(
    hangs.map((result) => result.status === 'unavailable' && result.reason),
    FINGERPRINT_PROBE_NAMES.map(() => 'timeout')
  )
})

test('optional probe failure does not fabricate or remove required evidence', async () => {
  const optional = await runFingerprintProbe('voices', () => Promise.reject(new Error('blocked')), { timeoutMs: 50 })
  assert.deepEqual(optional, { status: 'unavailable', probe: 'voices', reason: 'failed' })
  assert.deepEqual(
    collectRequiredFingerprintSignals(validEvidence(), [
      'lie_count',
      'headless_rating',
      'platform_consistent',
      'ua_consistent',
      'webgl_consistent',
    ]),
    {
      lie_count: '3',
      headless_rating: '17',
      platform_consistent: '1',
      ua_consistent: '1',
      webgl_consistent: '1',
    }
  )
})

test('every required signal fails closed instead of receiving a fabricated fallback', () => {
  const cases: Array<[string, (evidence: ReturnType<typeof validEvidence>) => void]> = [
    ['lie_count', (evidence) => delete (evidence as { lies?: unknown }).lies],
    ['headless_rating', (evidence) => delete (evidence as { headless?: unknown }).headless],
    ['platform_consistent', (evidence) => delete evidence.workerScope.platform],
    ['ua_consistent', (evidence) => delete evidence.navigator.system],
    ['webgl_consistent', (evidence) => delete evidence.workerScope.webglRenderer],
  ]

  for (const [signal, removeEvidence] of cases) {
    const evidence = validEvidence()
    removeEvidence(evidence)
    assert.throws(
      () => collectRequiredFingerprintSignals(evidence, [signal]),
      (error: unknown) =>
        error instanceof FingerprintCollectionError &&
        error.code === 'required-signal-unavailable' &&
        error.unavailableSignals[0] === signal
    )
  }

  assert.throws(
    () => collectRequiredFingerprintSignals(validEvidence(), ['future_signal']),
    (error: unknown) =>
      error instanceof FingerprintCollectionError && error.unavailableSignals.includes('future_signal')
  )
})

test('probe cancellation clears a hanging wait immediately', async () => {
  const controller = new AbortController()
  const pending = runFingerprintProbe('fonts', () => new Promise<never>(() => undefined), {
    signal: controller.signal,
    timeoutMs: 60_000,
  })
  controller.abort()
  await assert.rejects(pending, (error: unknown) => error instanceof Error && error.name === 'AbortError')
})

test('browser and Axios cancellation errors are recognized without hiding ordinary failures', () => {
  assert.equal(isAbortError(new DOMException('aborted', 'AbortError')), true)
  assert.equal(
    isAbortError(Object.assign(new Error('canceled'), { name: 'CanceledError', code: 'ERR_CANCELED' })),
    true
  )
  assert.equal(isAbortError(new Error('network failed')), false)
})
