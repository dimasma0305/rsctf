import assert from 'node:assert/strict'
import test, { type TestContext } from 'node:test'
import { createWsrxReadinessScheduler, WSRX_READINESS_POLL_MS, WSRX_READINESS_WINDOW_MS } from './WsrxReadiness'

const flushPromises = async () => {
  await Promise.resolve()
  await Promise.resolve()
}

const advanceImmediateWindow = async (context: TestContext, duration = WSRX_READINESS_WINDOW_MS) => {
  let advanced = 0
  while (advanced < duration) {
    const step = Math.min(WSRX_READINESS_POLL_MS, duration - advanced)
    context.mock.timers.tick(step)
    advanced += step
    await flushPromises()
  }
}

test('permanently unknown latency expires at the absolute deadline and stops polling', async (context) => {
  context.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 0 })
  let requests = 0
  let expired = new Set<string>()
  const scheduler = createWsrxReadinessScheduler({
    sync: async () => {
      requests += 1
    },
    onExpiredChange: (next) => {
      expired = new Set(next)
    },
  })

  try {
    scheduler.setEnabled(true)
    scheduler.updatePending(['unknown'])
    await advanceImmediateWindow(context)

    assert.equal(requests, 5)
    assert.deepEqual([...expired], ['unknown'])
    context.mock.timers.tick(60_000)
    await flushPromises()
    assert.equal(requests, 5)
  } finally {
    scheduler.dispose()
    context.mock.timers.reset()
  }
})

test('slow readiness responses are single-flight and schedule the next read from completion', async (context) => {
  context.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 0 })
  let requests = 0
  let maxInFlight = 0
  let inFlight = 0
  let resolveFirst: (() => void) | undefined
  const scheduler = createWsrxReadinessScheduler({
    sync: () => {
      requests += 1
      inFlight += 1
      maxInFlight = Math.max(maxInFlight, inFlight)
      if (requests === 1) {
        return new Promise<void>((resolve) => {
          resolveFirst = () => {
            inFlight -= 1
            resolve()
          }
        })
      }
      inFlight -= 1
      return Promise.resolve()
    },
    onExpiredChange: () => undefined,
  })

  try {
    scheduler.setEnabled(true)
    scheduler.updatePending(['slow'])
    context.mock.timers.tick(WSRX_READINESS_POLL_MS)
    assert.equal(requests, 1)

    context.mock.timers.tick(WSRX_READINESS_POLL_MS * 2)
    assert.equal(requests, 1)
    resolveFirst?.()
    await flushPromises()

    context.mock.timers.tick(WSRX_READINESS_POLL_MS - 1)
    assert.equal(requests, 1)
    context.mock.timers.tick(1)
    await flushPromises()
    assert.equal(requests, 2)
    assert.equal(maxInFlight, 1)
  } finally {
    scheduler.dispose()
    context.mock.timers.reset()
  }
})

test('daemon failure stops queued work and one explicit retry can recover', async (context) => {
  context.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 0 })
  let requests = 0
  let fail = true
  let expired = new Set<string>()
  let scheduler: ReturnType<typeof createWsrxReadinessScheduler>
  scheduler = createWsrxReadinessScheduler({
    sync: async () => {
      requests += 1
      if (fail) throw new Error('daemon offline')
      scheduler.updatePending([])
    },
    onExpiredChange: (next) => {
      expired = new Set(next)
    },
  })

  try {
    scheduler.setEnabled(true)
    scheduler.updatePending(['recoverable'])
    context.mock.timers.tick(WSRX_READINESS_POLL_MS)
    await flushPromises()
    assert.equal(requests, 1)
    assert.deepEqual([...expired], ['recoverable'])

    context.mock.timers.tick(30_000)
    assert.equal(requests, 1)
    fail = false
    scheduler.retry('recoverable')
    scheduler.retry('recoverable')
    context.mock.timers.tick(WSRX_READINESS_POLL_MS)
    await flushPromises()

    assert.equal(requests, 2)
    assert.equal(expired.size, 0)
    context.mock.timers.tick(30_000)
    assert.equal(requests, 2)
  } finally {
    scheduler.dispose()
    context.mock.timers.reset()
  }
})

test('provider unmount and option reset cancel queued accelerated work', async (context) => {
  context.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 0 })
  let requests = 0
  let expiryPublishes = 0
  let holdResponse = true
  let resolveResponse: (() => void) | undefined
  const scheduler = createWsrxReadinessScheduler({
    sync: () => {
      requests += 1
      if (!holdResponse) return Promise.resolve()
      return new Promise<void>((resolve) => {
        resolveResponse = resolve
      })
    },
    onExpiredChange: () => {
      expiryPublishes += 1
    },
  })

  try {
    scheduler.setEnabled(true)
    scheduler.updatePending(['old-options'])
    context.mock.timers.tick(WSRX_READINESS_POLL_MS)
    assert.equal(requests, 1)
    scheduler.reset()
    holdResponse = false
    resolveResponse?.()
    await flushPromises()
    context.mock.timers.tick(WSRX_READINESS_WINDOW_MS * 2)
    await flushPromises()
    assert.equal(requests, 1)
    assert.equal(expiryPublishes, 0)

    scheduler.setEnabled(true)
    scheduler.updatePending(['unmounted'])
    context.mock.timers.tick(WSRX_READINESS_POLL_MS)
    await flushPromises()
    assert.equal(requests, 2)
    scheduler.dispose()
    context.mock.timers.tick(WSRX_READINESS_WINDOW_MS * 2)
    await flushPromises()
    assert.equal(requests, 2)
    assert.equal(expiryPublishes, 0)
  } finally {
    scheduler.dispose()
    context.mock.timers.reset()
  }
})

test('one and one hundred pending tunnels share the same bounded request count', async (context) => {
  context.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 0 })

  const run = async (count: number) => {
    let requests = 0
    let expiredCount = 0
    const scheduler = createWsrxReadinessScheduler({
      sync: async () => {
        requests += 1
      },
      onExpiredChange: (expired) => {
        expiredCount = expired.size
      },
    })
    scheduler.setEnabled(true)
    scheduler.updatePending(Array.from({ length: count }, (_, index) => `remote-${index}`))
    await advanceImmediateWindow(context)
    const atDeadline = requests
    context.mock.timers.tick(60_000)
    await flushPromises()
    assert.equal(requests, atDeadline)
    scheduler.dispose()
    return { requests, expiredCount }
  }

  try {
    const one = await run(1)
    const hundred = await run(100)
    assert.equal(one.requests, hundred.requests)
    assert.equal(one.requests, 5)
    assert.equal(one.expiredCount, 1)
    assert.equal(hundred.expiredCount, 100)
  } finally {
    context.mock.timers.reset()
  }
})
