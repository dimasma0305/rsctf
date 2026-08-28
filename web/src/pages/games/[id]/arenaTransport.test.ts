import assert from 'node:assert/strict'
import test from 'node:test'
import { arenaRetryDelay, CompletionScheduledArenaCycle, retryAfterMilliseconds } from './arenaTransport'

const options = {
  successDelayMs: 15_000,
  failureBaseDelayMs: 1_000,
  maximumDelayMs: 60_000,
  requestTimeoutMs: 10_000,
  random: () => 0.5,
}

const settle = async () => {
  await Promise.resolve()
  await Promise.resolve()
}

test('arena polling never overlaps a slow completion-scheduled cycle', async (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'] })
  let calls = 0
  let complete: ((value: { success: boolean }) => void) | undefined
  const owner = new CompletionScheduledArenaCycle(
    () => {
      calls += 1
      return new Promise<{ success: boolean }>((resolve) => {
        complete = resolve
      })
    },
    { ...options, requestTimeoutMs: 120_000 }
  )

  owner.start()
  context.mock.timers.tick(0)
  await settle()
  context.mock.timers.tick(60_000)
  assert.equal(calls, 1)

  complete?.({ success: true })
  await settle()
  context.mock.timers.tick(14_999)
  assert.equal(calls, 1)
  context.mock.timers.tick(1)
  await settle()
  assert.equal(calls, 2)
  owner.stop()
})

test('arena polling aborts on timeout and retries with bounded jitter', async (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'] })
  let calls = 0
  const owner = new CompletionScheduledArenaCycle((signal) => {
    calls += 1
    return new Promise<{ success: boolean }>((_resolve, reject) => {
      signal.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')), { once: true })
    })
  }, options)

  owner.start()
  context.mock.timers.tick(0)
  await settle()
  context.mock.timers.tick(10_000)
  await settle()
  assert.equal(calls, 1)
  context.mock.timers.tick(499)
  assert.equal(calls, 1)
  context.mock.timers.tick(1)
  await settle()
  assert.equal(calls, 2)
  owner.stop()
})

test('arena polling aborts and stays idle while suspended, then resumes once', async (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'] })
  let calls = 0
  let aborts = 0
  const owner = new CompletionScheduledArenaCycle((signal) => {
    calls += 1
    return new Promise<{ success: boolean }>((_resolve, reject) => {
      signal.addEventListener(
        'abort',
        () => {
          aborts += 1
          reject(new DOMException('aborted', 'AbortError'))
        },
        { once: true }
      )
    })
  }, options)

  owner.start()
  context.mock.timers.tick(0)
  await settle()
  owner.suspend()
  await settle()
  context.mock.timers.tick(60_000)
  assert.equal(aborts, 1)
  assert.equal(calls, 1)

  owner.resume()
  context.mock.timers.tick(0)
  await settle()
  assert.equal(calls, 2)
  owner.stop()
})

test('stopping an arena owner aborts its only request and never schedules recovery', async (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'] })
  let calls = 0
  let aborts = 0
  const owner = new CompletionScheduledArenaCycle((signal) => {
    calls += 1
    return new Promise<{ success: boolean }>((_resolve, reject) => {
      signal.addEventListener(
        'abort',
        () => {
          aborts += 1
          reject(new DOMException('aborted', 'AbortError'))
        },
        { once: true }
      )
    })
  }, options)

  owner.start()
  context.mock.timers.tick(0)
  await settle()
  owner.stop()
  await settle()
  context.mock.timers.tick(120_000)
  await settle()

  assert.equal(calls, 1)
  assert.equal(aborts, 1)
})

test('arena polling honors Retry-After before retrying a failed cycle', async (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'] })
  let calls = 0
  const owner = new CompletionScheduledArenaCycle(async () => {
    calls += 1
    return calls === 1 ? { success: false, retryAfterMs: 12_000 } : { success: true }
  }, options)

  owner.start()
  context.mock.timers.tick(0)
  await settle()
  context.mock.timers.tick(11_999)
  assert.equal(calls, 1)
  context.mock.timers.tick(1)
  await settle()
  assert.equal(calls, 2)
  owner.stop()
})

test('arena retry policy parses Retry-After and caps jitter', () => {
  assert.equal(retryAfterMilliseconds('12', 1_000), 12_000)
  assert.equal(retryAfterMilliseconds(new Date(6_000).toUTCString(), 1_000), 5_000)
  assert.equal(retryAfterMilliseconds('999999', 1_000), 300_000)
  assert.equal(retryAfterMilliseconds('not-a-date', 1_000), undefined)
  assert.equal(
    arenaRetryDelay(1, 1_000, 60_000, () => 0),
    250
  )
  assert.equal(
    arenaRetryDelay(4, 1_000, 60_000, () => 0.5),
    4_000
  )
  assert.ok(arenaRetryDelay(99, 1_000, 60_000, () => 0.999) <= 60_000)
})
