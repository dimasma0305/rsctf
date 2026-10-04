import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import {
  GAME_TIMING_REFRESH_MS,
  GAME_TIMING_RETRY_CAP_MS,
  createGameTimingSWRConfig,
  gameTimingRetryDelay,
} from '../hooks/useGame'
import { installTestDom } from '../test/installDom'

const retryableError = { response: { status: 503 } }

test('timing retries grow exponentially, stop at the cap, and remain isolated by key', (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const owner = createGameTimingSWRConfig(
    () => 0,
    () => 1
  )
  const activeConfig = { ...owner.config, isOnline: () => true, isVisible: () => true }
  const fired: string[] = []
  const unsubscribes: Array<() => void> = []

  try {
    assert.deepEqual(
      [1, 2, 3, 4, 40].map((retryCount) => gameTimingRetryDelay(retryCount, () => 1)),
      [60_000, 120_000, 240_000, GAME_TIMING_RETRY_CAP_MS, GAME_TIMING_RETRY_CAP_MS]
    )

    for (const retryCount of [1, 2, 3, 4, 40]) {
      const key = `/timing/${retryCount}`
      unsubscribes.push(owner.subscribe(key, () => undefined))
      owner.config.onErrorRetry?.(
        retryableError,
        key,
        activeConfig,
        () => {
          fired.push(key)
        },
        { retryCount }
      )
    }

    const cancelledKey = '/timing/cancelled'
    unsubscribes.push(owner.subscribe(cancelledKey, () => undefined))
    owner.config.onErrorRetry?.(
      retryableError,
      cancelledKey,
      activeConfig,
      () => {
        fired.push(cancelledKey)
      },
      { retryCount: 1 }
    )
    owner.config.onSuccess?.({}, cancelledKey, activeConfig)

    context.mock.timers.tick(GAME_TIMING_REFRESH_MS)
    assert.deepEqual(fired, ['/timing/1'])
    context.mock.timers.tick(GAME_TIMING_REFRESH_MS)
    assert.deepEqual(fired, ['/timing/1', '/timing/2'])
    context.mock.timers.tick(GAME_TIMING_REFRESH_MS * 2)
    assert.deepEqual(fired, ['/timing/1', '/timing/2', '/timing/3'])
    context.mock.timers.tick(GAME_TIMING_RETRY_CAP_MS - GAME_TIMING_REFRESH_MS * 4)
    assert.deepEqual(fired, ['/timing/1', '/timing/2', '/timing/3', '/timing/4', '/timing/40'])
  } finally {
    unsubscribes.forEach((unsubscribe) => unsubscribe())
    owner.cancelAll()
    context.mock.timers.reset()
  }
})

test('equal jitter de-synchronizes clients with the same retry count', (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const earlyClient = createGameTimingSWRConfig(
    () => 0,
    () => 0
  )
  const lateClient = createGameTimingSWRConfig(
    () => 0,
    () => 1
  )
  const earlyConfig = { ...earlyClient.config, isOnline: () => true, isVisible: () => true }
  const lateConfig = { ...lateClient.config, isOnline: () => true, isVisible: () => true }
  const fired: string[] = []
  const unsubscribeEarly = earlyClient.subscribe('/api/game/9', () => undefined)
  const unsubscribeLate = lateClient.subscribe('/api/game/9', () => undefined)

  try {
    earlyClient.config.onErrorRetry?.(
      retryableError,
      '/api/game/9',
      earlyConfig,
      () => {
        fired.push('early')
      },
      { retryCount: 1 }
    )
    lateClient.config.onErrorRetry?.(
      retryableError,
      '/api/game/9',
      lateConfig,
      () => {
        fired.push('late')
      },
      { retryCount: 1 }
    )

    context.mock.timers.tick(GAME_TIMING_REFRESH_MS / 2 - 1)
    assert.deepEqual(fired, [])
    context.mock.timers.tick(1)
    assert.deepEqual(fired, ['early'])
    context.mock.timers.tick(GAME_TIMING_REFRESH_MS / 2 - 1)
    assert.deepEqual(fired, ['early'])
    context.mock.timers.tick(1)
    assert.deepEqual(fired, ['early', 'late'])
  } finally {
    unsubscribeEarly()
    unsubscribeLate()
    earlyClient.cancelAll()
    lateClient.cancelAll()
    context.mock.timers.reset()
  }
})

test('timing retries wait for visibility and connectivity without reviving an obsolete key', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  let visibilityState: DocumentVisibilityState = 'hidden'
  let online = false
  Object.defineProperty(browser.document, 'visibilityState', {
    configurable: true,
    get: () => visibilityState,
  })
  Object.defineProperty(browser.navigator, 'onLine', {
    configurable: true,
    get: () => online,
  })
  const owner = createGameTimingSWRConfig(
    () => 0,
    () => 0
  )
  const inactiveConfig = {
    ...owner.config,
    isOnline: () => browser.navigator.onLine,
    isVisible: () => browser.document.visibilityState !== 'hidden',
  }
  const fired: string[] = []
  const obsoleteKey = '/timing/obsolete'
  const activeKey = '/timing/active'
  const unsubscribeObsolete = owner.subscribe(obsoleteKey, () => undefined)
  const unsubscribeActive = owner.subscribe(activeKey, () => undefined)

  try {
    for (const key of [obsoleteKey, activeKey]) {
      owner.config.onErrorRetry?.(
        retryableError,
        key,
        inactiveConfig,
        () => {
          fired.push(key)
        },
        { retryCount: 1 }
      )
    }

    context.mock.timers.tick(GAME_TIMING_REFRESH_MS / 2)
    assert.deepEqual(fired, [])
    unsubscribeObsolete()

    visibilityState = 'visible'
    browser.document.dispatchEvent(new browser.Event('visibilitychange'))
    assert.deepEqual(fired, [])

    online = true
    browser.dispatchEvent(new browser.Event('online'))
    assert.deepEqual(fired, [activeKey])
  } finally {
    unsubscribeActive()
    owner.cancelAll()
    context.mock.timers.reset()
    await browser.happyDOM.close()
    restoreDom()
  }
})
