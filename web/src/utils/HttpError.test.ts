import assert from 'node:assert/strict'
import test from 'node:test'
import { boundedRetryDelay, httpErrorStatus, retryAfterMilliseconds } from './HttpError'

test('HTTP status extraction recognizes generated and Axios errors', () => {
  assert.equal(httpErrorStatus({ status: 403 }), 403)
  assert.equal(httpErrorStatus({ response: { status: 429 } }), 429)
  assert.equal(httpErrorStatus(new Error('403')), null)
})

test('bounded retries stop terminal and exhausted reads', () => {
  assert.equal(boundedRetryDelay({ response: { status: 403 } }, 0), null)
  assert.equal(boundedRetryDelay({ response: { status: 404 } }, 0), null)
  assert.equal(boundedRetryDelay({ response: { status: 500 } }, 3), null)
  assert.equal(boundedRetryDelay({ response: { status: 500 } }, 0, () => 0), 500)
})

test('Retry-After controls throttled recovery and remains bounded', () => {
  assert.equal(retryAfterMilliseconds({ response: { headers: { 'retry-after': '7' } } }), 7_000)
  assert.equal(boundedRetryDelay({ response: { status: 429, headers: { 'retry-after': '7' } } }, 0), 7_000)
  assert.equal(
    boundedRetryDelay({ response: { status: 429, headers: { 'retry-after': '999999' } } }, 0),
    300_000
  )
})
