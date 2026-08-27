import assert from 'node:assert/strict'
import test from 'node:test'
import {
  assertJsonResponse,
  challengePollRetryDelay,
  createChallengePollOwner,
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
