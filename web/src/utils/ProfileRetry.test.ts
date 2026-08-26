import assert from 'node:assert/strict'
import test from 'node:test'
import {
  MAX_PROFILE_RETRIES,
  PROFILE_RECOVERY_PROBE_MS,
  createProfileRetryTimers,
  profileErrorDisposition,
  profileRetryScheduleDelay,
  profileRetryDelay,
  retryAfterMilliseconds,
} from './ProfileRetry'

test('profile errors separate terminal sessions from bounded transient recovery', () => {
  assert.equal(profileErrorDisposition(null), 'stop')
  assert.equal(profileErrorDisposition(undefined), 'stop')
  assert.equal(profileErrorDisposition({ response: { status: 401 } }), 'anonymous')
  assert.equal(profileErrorDisposition({ status: 403 }), 'banned')
  assert.equal(profileErrorDisposition({ response: { status: 404 } }), 'stop')
  assert.equal(profileErrorDisposition({ response: { status: 503 } }), 'retry')
  assert.equal(profileErrorDisposition(new TypeError('offline')), 'retry')

  const limited = { response: { status: 429, headers: { 'retry-after': '12' } } }
  assert.equal(retryAfterMilliseconds(limited, 1_000), 12_000)
  assert.equal(
    profileRetryDelay(limited, 0, () => 0, 1_000),
    12_000
  )
  assert.equal(
    profileRetryDelay(limited, MAX_PROFILE_RETRIES, () => 0, 1_000),
    null
  )

  const excessive = { response: { status: 429, headers: { 'retry-after': '3600' } } }
  assert.equal(
    profileRetryDelay(excessive, 0, () => 0, 1_000),
    null
  )
  assert.equal(
    profileRetryScheduleDelay(limited, MAX_PROFILE_RETRIES, () => 0, 1_000),
    PROFILE_RECOVERY_PROBE_MS
  )
  assert.equal(profileRetryScheduleDelay(excessive, 0, () => 0, 1_000), 60 * 60_000)
  assert.equal(profileRetryScheduleDelay({ response: { status: 404 } }, MAX_PROFILE_RETRIES), null)

  const serverDate = Date.parse('Wed, 26 Aug 2026 20:00:00 GMT')
  const retryDate = new Date(serverDate + 30_000).toUTCString()
  const datedLimit = {
    response: {
      status: 429,
      headers: { date: new Date(serverDate).toUTCString(), 'retry-after': retryDate },
    },
  }
  assert.equal(retryAfterMilliseconds(datedLimit, serverDate + 2 * 60 * 60_000), 30_000)
  assert.equal(retryAfterMilliseconds(datedLimit, serverDate - 2 * 60 * 60_000), 30_000)
})

test('profile retry timers retain only the latest retry and cancel after recovery', (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const retries = createProfileRetryTimers()
  const calls: string[] = []

  try {
    retries.schedule(1_000, () => {
      calls.push('superseded')
    })
    assert.equal(retries.pending(), 1)
    retries.schedule(500, () => {
      calls.push('latest')
    })
    assert.equal(retries.pending(), 1)
    context.mock.timers.tick(1_500)
    assert.deepEqual(calls, ['latest'])
    assert.equal(retries.pending(), 0)

    // A successful same-user response cancels the retry even though the
    // account identity did not change.
    retries.schedule(1_000, () => {
      calls.push('after recovery')
    })
    retries.cancel()
    context.mock.timers.tick(2_000)
    assert.deepEqual(calls, ['latest'])
    assert.equal(retries.pending(), 0)

    const recoveryDelay = profileRetryScheduleDelay(
      { response: { status: 503 } },
      MAX_PROFILE_RETRIES,
      () => 0,
      0
    )
    assert.equal(recoveryDelay, PROFILE_RECOVERY_PROBE_MS)
    retries.schedule(recoveryDelay ?? 0, () => {
      calls.push('recovered after cap')
    })
    context.mock.timers.tick(PROFILE_RECOVERY_PROBE_MS - 1)
    assert.deepEqual(calls, ['latest'])
    context.mock.timers.tick(1)
    assert.deepEqual(calls, ['latest', 'recovered after cap'])
  } finally {
    retries.cancel()
    context.mock.timers.reset()
  }
})
