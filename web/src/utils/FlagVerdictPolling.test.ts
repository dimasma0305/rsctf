import assert from 'node:assert/strict'
import test from 'node:test'
import { AnswerResult } from '@Api'
import {
  MAX_FLAG_VERDICT_DELAY_MS,
  MAX_FLAG_VERDICT_FAILURES,
  createFlagVerdictPoller,
  flagVerdictFailureDelay,
  flagVerdictPendingDelay,
  sameFlagVerdictIdentity,
  type FlagVerdictIdentity,
} from './FlagVerdictPolling'

const flush = async () => {
  await Promise.resolve()
  await Promise.resolve()
}

const deferred = <T>() => {
  let resolve!: (value: T) => void
  let reject!: (error: unknown) => void
  const promise = new Promise<T>((onResolve, onReject) => {
    resolve = onResolve
    reject = onReject
  })
  return { promise, resolve, reject }
}

const identity = (gameId: number, challengeId: number, submissionId: number): FlagVerdictIdentity => ({
  gameId,
  challengeId,
  submissionId,
})

test('verdict polling serializes slow reads and bounds pending cadence', async (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const first = deferred<AnswerResult>()
  const second = deferred<AnswerResult>()
  let calls = 0
  let active = 0
  let maximumActive = 0
  const poller = createFlagVerdictPoller({
    identity: identity(1, 2, 3),
    request: async () => {
      calls += 1
      active += 1
      maximumActive = Math.max(maximumActive, active)
      try {
        return await (calls === 1 ? first.promise : second.promise)
      } finally {
        active -= 1
      }
    },
    onTerminal: () => undefined,
    onFailure: () => undefined,
  })

  try {
    poller.start()
    context.mock.timers.tick(60_000)
    assert.equal(calls, 1, 'a slow request must not be overlapped by interval ticks')

    first.resolve(AnswerResult.FlagSubmitted)
    await flush()
    context.mock.timers.tick(flagVerdictPendingDelay(0) - 1)
    assert.equal(calls, 1)
    context.mock.timers.tick(1)
    assert.equal(calls, 2)
    assert.equal(maximumActive, 1)

    second.resolve(AnswerResult.FlagSubmitted)
    await flush()
    assert.equal(flagVerdictPendingDelay(99), MAX_FLAG_VERDICT_DELAY_MS)
  } finally {
    poller.cancel()
    context.mock.timers.reset()
  }
})

test('one transient failure retries without publishing a failure or duplicate terminal callback', async (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const seen: Array<[FlagVerdictIdentity, AnswerResult]> = []
  const failures: unknown[] = []
  let calls = 0
  const current = identity(4, 5, 6)
  const poller = createFlagVerdictPoller({
    identity: current,
    request: async () => {
      calls += 1
      if (calls === 1) throw { response: { status: 503 } }
      return AnswerResult.Accepted
    },
    onTerminal: (key, result) => seen.push([key, result]),
    onFailure: (_key, error) => failures.push(error),
    random: () => 0.5,
  })

  try {
    poller.start()
    poller.start()
    await flush()
    assert.equal(calls, 1)
    assert.equal(poller.pending(), true, 'a recoverable failure must retain pending ownership')
    context.mock.timers.tick(flagVerdictFailureDelay({ response: { status: 503 } }, 1, () => 0.5))
    await flush()
    assert.equal(calls, 2)
    assert.deepEqual(seen, [[current, AnswerResult.Accepted]])
    assert.deepEqual(failures, [])
    context.mock.timers.tick(60_000)
    await flush()
    assert.equal(calls, 2)
    assert.equal(seen.length, 1)
  } finally {
    poller.cancel()
    context.mock.timers.reset()
  }
})

test('verdict recovery honors the server ceiling without creating an unbounded timer', () => {
  const limited = { response: { status: 429, headers: { 'retry-after': '3600' } } }
  assert.equal(
    flagVerdictFailureDelay(limited, 1, () => 0.5),
    60_000
  )
})

test('transient retry exhaustion is bounded and reports once', async (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  let calls = 0
  let failures = 0
  const error = { response: { status: 503 } }
  const poller = createFlagVerdictPoller({
    identity: identity(7, 8, 9),
    request: async () => {
      calls += 1
      throw error
    },
    onTerminal: () => assert.fail('a failed recovery must not publish a verdict'),
    onFailure: (_key, received) => {
      failures += 1
      assert.equal(received, error)
    },
    random: () => 0.5,
  })

  try {
    poller.start()
    for (let failure = 1; failure < MAX_FLAG_VERDICT_FAILURES; failure += 1) {
      await flush()
      context.mock.timers.tick(flagVerdictFailureDelay(error, failure, () => 0.5))
    }
    await flush()
    assert.equal(calls, MAX_FLAG_VERDICT_FAILURES)
    assert.equal(failures, 1)
    assert.equal(poller.pending(), false)
  } finally {
    poller.cancel()
    context.mock.timers.reset()
  }
})

test('modal unmount and route change cancel pending reads before they can publish', async () => {
  const pending = deferred<AnswerResult>()
  const callbacks: string[] = []
  const oldIdentity = identity(10, 11, 12)
  const poller = createFlagVerdictPoller({
    identity: oldIdentity,
    request: (_key, signal) => {
      signal.addEventListener('abort', () => callbacks.push('aborted'), { once: true })
      return pending.promise
    },
    onTerminal: () => callbacks.push('terminal'),
    onFailure: () => callbacks.push('failure'),
  })

  poller.start()
  poller.cancel()
  pending.resolve(AnswerResult.Accepted)
  await flush()
  assert.deepEqual(callbacks, ['aborted'])
  assert.equal(poller.pending(), false)
})

test('closing A and opening B cannot mix identities or publish A into B', async () => {
  const oldResponse = deferred<AnswerResult>()
  const results: Array<[FlagVerdictIdentity, AnswerResult]> = []
  const challengeA = identity(13, 21, 34)
  const challengeB = identity(13, 22, 35)
  const pollA = createFlagVerdictPoller({
    identity: challengeA,
    request: () => oldResponse.promise,
    onTerminal: (key, result) => results.push([key, result]),
    onFailure: () => undefined,
  })
  const pollB = createFlagVerdictPoller({
    identity: challengeB,
    request: async () => AnswerResult.WrongAnswer,
    onTerminal: (key, result) => results.push([key, result]),
    onFailure: () => undefined,
  })

  assert.equal(sameFlagVerdictIdentity(challengeA, challengeB), false)
  pollA.start()
  pollA.cancel()
  pollB.start()
  oldResponse.resolve(AnswerResult.Accepted)
  await flush()

  assert.deepEqual(results, [[challengeB, AnswerResult.WrongAnswer]])
})
