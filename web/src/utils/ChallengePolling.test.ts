import assert from 'node:assert/strict'
import test from 'node:test'
import {
  assertJsonResponse,
  captureChallengeReadFailure,
  challengePollRetryDelay,
  challengeReadFailure,
  createChallengePollOwner,
  createChallengeRecoveryOwner,
  createChallengeRequestId,
  isAbortError,
  isChallengePollRetryable,
  NonJsonResponseError,
} from './ChallengePolling'

test('challenge polling stops on permanent statuses and an HTML SPA response', () => {
  for (const status of [401, 403, 404]) {
    assert.equal(isChallengePollRetryable({ response: { status } }), false)
    assert.equal(challengePollRetryDelay({ response: { status } }, 0), null)
  }
  assert.throws(
    () => assertJsonResponse({ status: 200, data: '<!doctype html>', headers: { 'content-type': 'text/html' } }),
    NonJsonResponseError
  )
})

test('challenge polling bounds transient retries and honors Retry-After', () => {
  const limited = { response: { status: 429, headers: { 'retry-after': '12' } } }
  assert.equal(
    challengePollRetryDelay(limited, 0, () => 0, 0),
    12_000
  )
  assert.equal(
    challengePollRetryDelay(limited, 3, () => 0, 0),
    null
  )
  assert.equal(
    challengePollRetryDelay({ response: { status: 429, headers: { 'retry-after': '3600' } } }, 0, () => 0, 0),
    null
  )
  assert.equal(isChallengePollRetryable({ response: { status: 503 } }), true)
})

test('challenge poll owner keeps one request and one retry timer', () => {
  const owner = createChallengePollOwner()
  const first = owner.begin()
  const second = owner.begin()
  assert.equal(first.signal.aborted, true)
  assert.equal(second.signal.aborted, false)

  owner.schedule(60_000, () => undefined)
  owner.schedule(60_000, () => undefined)
  assert.equal(owner.pendingRetryCount(), 1)
  owner.cancel()
  assert.equal(second.signal.aborted, true)
  assert.equal(owner.pendingRetryCount(), 0)
  assert.equal(isAbortError({ code: 'ERR_CANCELED' }), true)
})

test('valid JSON response content types remain accepted', () => {
  const data = { ok: true }
  assert.equal(
    assertJsonResponse({ status: 200, data, headers: { 'content-type': 'application/problem+json; charset=utf-8' } }),
    data
  )
})

test('challenge read diagnostics preserve the typed error and expose only safe references', () => {
  const error = {
    response: {
      status: 429,
      headers: {
        'retry-after': '12',
        'x-request-id': 'server-trace-018f47d2',
        authorization: 'Bearer must-never-appear',
      },
    },
  }
  const requestId = createChallengeRequestId('solvers')
  assert.match(requestId, /^challenge-solvers-[A-Za-z0-9-]+$/)
  assert.equal(captureChallengeReadFailure(error, 'solvers', requestId), error)
  assert.deepEqual(challengeReadFailure(error), {
    resource: 'solvers',
    requestId,
    serverTraceId: 'server-trace-018f47d2',
    retryAfterMilliseconds: 12_000,
  })

  const unsafe = { response: { status: 503, headers: { 'x-request-id': 'secret/bearer?query' } } }
  captureChallengeReadFailure(unsafe, 'challenge', 'challenge-detail-safe-id')
  assert.equal(challengeReadFailure(unsafe)?.serverTraceId, undefined)
  assert.doesNotMatch(JSON.stringify(challengeReadFailure(error)), /Bearer|authorization|must-never/)
})

test('related challenge reads retain independent deadlines behind one recovery timer', (context) => {
  context.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 0 })
  const owner = createChallengeRecoveryOwner()
  const recovered: string[] = []
  try {
    owner.schedule('detail', 1_000, () => recovered.push('detail'))
    owner.schedule('solvers', 1_000, () => recovered.push('solvers'))
    assert.equal(owner.pendingEntryCount(), 2)
    assert.equal(owner.pendingTimerCount(), 1)
    context.mock.timers.tick(999)
    assert.deepEqual(recovered, [])
    context.mock.timers.tick(1)
    assert.deepEqual(recovered.sort(), ['detail', 'solvers'])
    assert.equal(owner.pendingEntryCount(), 0)
    assert.equal(owner.pendingTimerCount(), 0)
  } finally {
    owner.cancelAll()
    context.mock.timers.reset()
  }
})
