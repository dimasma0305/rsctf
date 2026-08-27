import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, type FC } from 'react'
import { installTestDom } from '../test/installDom'

test('server-corrected lifecycle crosses kickoff and close without navigation', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/1' })
  const restoreDom = installTestDom(browser)
  const localStart = 2_000_003_600_000
  const serverStart = localStart - 60 * 60 * 1000
  context.mock.timers.enable({
    apis: ['Date', 'setInterval', 'setTimeout'],
    now: new Date(localStart),
  })
  const { getServerNowMilliseconds, observeServerTime, serverClockTestApi } = await import('./ServerClock')
  const { GAME_TIMING_REFRESH_MS, gameTimingSWRConfig, getGameStatus, useGameStatus } = await import('../hooks/useGame')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  let game = { start: serverStart + 500, end: serverStart + 2_500 }
  const laterGame = { start: serverStart + 5_000, end: serverStart + 8_000 }
  const Probe: FC = () => {
    const { status, now } = useGameStatus(game)
    const statuses = [game, laterGame].map((item) => getGameStatus(item, now).status)
    const live = statuses.filter((item) => item === 'ongoing').length
    const upcoming = statuses.filter((item) => item === 'coming').length
    return createElement('output', null, `${status}:${live}:${upcoming}`)
  }

  try {
    serverClockTestApi.reset()
    assert.equal(observeServerTime(serverStart, localStart), true)
    assert.equal(serverClockTestApi.offset(), -60 * 60 * 1000)
    assert.equal(getServerNowMilliseconds(localStart), serverStart)
    assert.equal(serverClockTestApi.modelServerTime({ data: [{ serverTime: serverStart }] }), serverStart)
    assert.equal(GAME_TIMING_REFRESH_MS, 60_000)
    assert.equal(gameTimingSWRConfig.refreshInterval, GAME_TIMING_REFRESH_MS)
    assert.equal(gameTimingSWRConfig.refreshWhenHidden, false)
    assert.equal(gameTimingSWRConfig.refreshWhenOffline, false)

    await act(async () => root.render(createElement(Probe)))
    assert.equal(container.textContent, 'coming:0:2')

    await act(async () => context.mock.timers.tick(1_000))
    assert.equal(container.textContent, 'ongoing:1:1')

    await act(async () => context.mock.timers.tick(2_000))
    assert.equal(container.textContent, 'ended:0:1')

    // A live schedule edit must update the retained page immediately. A late,
    // older HTTP response cannot roll the corrected clock backwards afterward.
    game = { ...game, end: serverStart + 10_000 }
    await act(async () => root.render(createElement(Probe)))
    assert.equal(container.textContent, 'ongoing:1:1')
    assert.equal(observeServerTime(serverStart - 1, localStart + 3_000), false)
    assert.equal(serverClockTestApi.offset(), -60 * 60 * 1000)

    await act(async () => root.unmount())

    serverClockTestApi.reset()
    assert.equal(observeServerTime(serverStart, localStart + 200, localStart), true)
    assert.equal(serverClockTestApi.offset(), -60 * 60 * 1000 - 200)
    assert.equal(serverClockTestApi.bestRoundTrip(), 200)

    // A newer low-latency response corrects the estimate. A later slow
    // response advances ordering but cannot degrade the offset in this window.
    assert.equal(observeServerTime(serverStart + 1_000, localStart + 1_020, localStart + 1_000), true)
    assert.equal(serverClockTestApi.offset(), -60 * 60 * 1000 - 20)
    assert.equal(serverClockTestApi.bestRoundTrip(), 20)
    assert.equal(observeServerTime(serverStart + 2_000, localStart + 4_000, localStart + 2_000), true)
    assert.equal(serverClockTestApi.offset(), -60 * 60 * 1000 - 20)

    // The API stamps serverTime after handler work. Ten seconds of server-side
    // processing must not be interpreted as five seconds of positive skew.
    serverClockTestApi.reset()
    assert.equal(observeServerTime(serverStart + 10_000, localStart + 10_020, localStart), true)
    assert.equal(serverClockTestApi.offset(), -60 * 60 * 1000 - 20)
  } finally {
    serverClockTestApi.reset()
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('cached event access waits for a live clock sample before enforcing lifecycle state', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/challenges' })
  const restoreDom = installTestDom(browser)
  const localNow = 2_000_007_200_000
  const serverNow = localNow - 2 * 60 * 60_000
  context.mock.timers.enable({
    apis: ['Date', 'setInterval', 'setTimeout'],
    now: new Date(localNow),
  })
  const {
    hasLiveServerClockSample,
    observeServerTime,
    serverClockTestApi,
    useServerClockReady,
  } = await import('./ServerClock')
  const { useGameStatus } = await import('../hooks/useGame')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const cachedGame = { start: serverNow - 500, end: serverNow + 500 }
  const Probe: FC = () => {
    const ready = useServerClockReady()
    const { status } = useGameStatus(cachedGame)
    return createElement('output', null, ready ? status : 'waiting')
  }
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    serverClockTestApi.reset()
    assert.equal(hasLiveServerClockSample(), false)
    await act(async () => root.render(createElement(Probe)))
    assert.equal(container.textContent, 'waiting')

    await act(async () => {
      assert.equal(observeServerTime(serverNow, localNow), true)
    })
    assert.equal(hasLiveServerClockSample(), true)
    assert.equal(container.textContent, 'ongoing')
  } finally {
    await act(async () => root.unmount())
    serverClockTestApi.reset()
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('timing polling owns one retry per subscribed key and cancels it after recovery or unmount', async (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const { createGameTimingSWRConfig, GAME_TIMING_REFRESH_MS, shouldRetryGameTimingError } = await import(
    '../hooks/useGame'
  )
  const owner = createGameTimingSWRConfig(
    () => 0,
    () => 1
  )
  const { config } = owner
  const activeConfig = { ...config, isOnline: () => true, isVisible: () => true }
  let supersededReads = 0
  let recoveredReads = 0
  const options = { retryCount: 1 }

  try {
    assert.equal(shouldRetryGameTimingError({ response: { status: 503 } }), true)
    assert.equal(shouldRetryGameTimingError({ response: { status: 404 } }), false)
    const unsubscribe = owner.subscribe('/api/game/recent', () => undefined)
    config.onErrorRetry?.(
      { response: { status: 503 } },
      '/api/game/recent',
      activeConfig,
      () => {
        supersededReads += 1
      },
      options
    )
    config.onErrorRetry?.(
      { response: { status: 503 } },
      '/api/game/recent',
      activeConfig,
      () => {
        recoveredReads += 1
      },
      options
    )
    context.mock.timers.tick(GAME_TIMING_REFRESH_MS - 1)
    assert.equal(supersededReads, 0)
    assert.equal(recoveredReads, 0)
    context.mock.timers.tick(1)
    assert.equal(supersededReads, 0)
    assert.equal(recoveredReads, 1)

    config.onErrorRetry?.(
      { response: { status: 503 } },
      '/api/game/recent',
      activeConfig,
      () => {
        recoveredReads += 1
      },
      options
    )
    config.onSuccess?.([], '/api/game/recent', config)
    context.mock.timers.tick(GAME_TIMING_REFRESH_MS)
    assert.equal(recoveredReads, 1)

    config.onErrorRetry?.(
      { response: { status: 503 } },
      '/api/game/recent',
      activeConfig,
      () => {
        recoveredReads += 1
      },
      options
    )
    owner.cancelAll()
    context.mock.timers.tick(GAME_TIMING_REFRESH_MS)
    assert.equal(recoveredReads, 1)
    unsubscribe()
  } finally {
    owner.cancelAll()
    context.mock.timers.reset()
  }
})

test('multiple game subscribers share one refresh request per timing key', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/17/scoreboard' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const { GAME_TIMING_REFRESH_MS, useGame, useGameScoreboard, useGameTeamInfo } = await import('../hooks/useGame')
  const { SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  let reads = 0
  const cache = new Map()
  const swrConfig = {
    provider: () => cache,
    fetcher: async (request: unknown) => {
      const path = typeof request === 'string' ? request : Array.isArray(request) ? request[0] : null
      if (path === '/api/game/17') reads += 1
      return { id: 17, start: 0, end: GAME_TIMING_REFRESH_MS * 10 }
    },
  }
  const DirectGameSubscriber: FC = () => {
    useGame(17)
    return null
  }
  const TeamInfoSubscriber: FC = () => {
    useGameTeamInfo(17, false)
    return null
  }
  const ScoreboardSubscriber: FC = () => {
    useGameScoreboard(17, false)
    return null
  }
  const subscribers = {
    direct: DirectGameSubscriber,
    team: TeamInfoSubscriber,
    scoreboard: ScoreboardSubscriber,
  }
  type Subscriber = keyof typeof subscribers
  const ScoreboardSubscribers: FC<{ active: Subscriber[] }> = ({ active }) =>
    createElement(
      SWRConfig,
      {
        value: swrConfig,
      },
      ...active.map((name) => createElement(subscribers[name], { key: name }))
    )
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(ScoreboardSubscribers, { active: ['direct'] })))
    assert.equal(reads, 1)

    // Scoreboard panels and timelines do not all mount on the same render. Their
    // individual SWR refresh timers used to drift and each request this key.
    await act(async () => context.mock.timers.tick(10_000))
    await act(async () => root.render(createElement(ScoreboardSubscribers, { active: ['direct', 'team'] })))
    await act(async () => context.mock.timers.tick(10_000))
    await act(async () =>
      root.render(createElement(ScoreboardSubscribers, { active: ['direct', 'team', 'scoreboard'] }))
    )
    const readsAfterMount = reads

    await act(async () => context.mock.timers.tick(GAME_TIMING_REFRESH_MS))
    assert.equal(reads, readsAfterMount + 1)

    // The remaining indirect useGame subscriber takes over when the original
    // owner disappears, without reviving a second timer.
    await act(async () => root.render(createElement(ScoreboardSubscribers, { active: ['team', 'scoreboard'] })))
    const readsBeforeHandoff = reads
    await act(async () => context.mock.timers.tick(GAME_TIMING_REFRESH_MS))
    assert.equal(reads, readsBeforeHandoff + 1)

    await act(async () => root.render(createElement(ScoreboardSubscribers, { active: [] })))
    const readsAfterStop = reads
    await act(async () => context.mock.timers.tick(GAME_TIMING_REFRESH_MS))
    assert.equal(reads, readsAfterStop)
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('timing retries pause while hidden or offline and cancel across scope changes', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  let visibilityState: DocumentVisibilityState = 'visible'
  let online = true
  Object.defineProperty(browser.document, 'visibilityState', {
    configurable: true,
    get: () => visibilityState,
  })
  Object.defineProperty(browser.navigator, 'onLine', {
    configurable: true,
    get: () => online,
  })
  const { GAME_TIMING_REFRESH_MS, useGameTimingSWRConfig } = await import('../hooks/useGame')
  const { default: useSWR, SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const reads = new Map<number, number>()
  const cache = new Map()
  const swrConfig = {
    provider: () => cache,
    isVisible: () => browser.document.visibilityState !== 'hidden',
    isOnline: () => browser.navigator.onLine,
  }
  const Probe: FC<{ page: number }> = ({ page }) => {
    const timingConfig = useGameTimingSWRConfig()
    useSWR(
      ['/test/inactive-timing', { page }],
      async ([, query]) => {
        const count = (reads.get(query.page) ?? 0) + 1
        reads.set(query.page, count)
        if (count === 1) throw { response: { status: 503 } }
        return { page: query.page }
      },
      timingConfig
    )
    return null
  }
  const Scope: FC<{ page: number | null }> = ({ page }) =>
    createElement(SWRConfig, { value: swrConfig }, page === null ? null : createElement(Probe, { page }))
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(Scope, { page: 1 })))
    assert.equal(reads.get(1), 1)

    visibilityState = 'hidden'
    await act(async () => context.mock.timers.tick(GAME_TIMING_REFRESH_MS))
    assert.equal(reads.get(1), 1)
    await act(async () => context.mock.timers.tick(GAME_TIMING_REFRESH_MS * 3))
    assert.equal(reads.get(1), 1)

    visibilityState = 'visible'
    online = false
    await act(async () => {
      browser.document.dispatchEvent(new browser.Event('visibilitychange'))
    })
    assert.equal(reads.get(1), 1)

    online = true
    await act(async () => {
      browser.document.dispatchEvent(new browser.Event('visibilitychange'))
      browser.dispatchEvent(new browser.Event('online'))
    })
    assert.equal(reads.get(1), 2)

    // Switching keys cancels page 2 after its inactive timeout has become a
    // deferred retry. A later activity signal cannot revive the old key.
    await act(async () => root.render(createElement(Scope, { page: 2 })))
    assert.equal(reads.get(2), 1)
    visibilityState = 'hidden'
    await act(async () => context.mock.timers.tick(GAME_TIMING_REFRESH_MS))
    assert.equal(reads.get(2), 1)
    await act(async () => root.render(createElement(Scope, { page: 3 })))
    assert.equal(reads.get(3), 1)
    visibilityState = 'visible'
    await act(async () => {
      browser.document.dispatchEvent(new browser.Event('visibilitychange'))
      browser.dispatchEvent(new browser.Event('online'))
    })
    assert.equal(reads.get(2), 1)
    assert.equal(reads.get(3), 1)

    // The last unmount also removes a retry that was already deferred while
    // hidden, including its shared activity listeners.
    await act(async () => root.render(createElement(Scope, { page: 4 })))
    assert.equal(reads.get(4), 1)
    visibilityState = 'hidden'
    await act(async () => context.mock.timers.tick(GAME_TIMING_REFRESH_MS))
    assert.equal(reads.get(4), 1)
    await act(async () => root.render(createElement(Scope, { page: null })))
    visibilityState = 'visible'
    await act(async () => {
      browser.document.dispatchEvent(new browser.Event('visibilitychange'))
      browser.dispatchEvent(new browser.Event('online'))
      context.mock.timers.tick(GAME_TIMING_REFRESH_MS)
    })
    assert.equal(reads.get(4), 1)
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('timing polling cancels an obsolete scope before its retry fires', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const { GAME_TIMING_REFRESH_MS, useGameTimingSWRConfig } = await import('../hooks/useGame')
  const { default: useSWR } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const reads = new Map<number, number>()
  const Probe: FC<{ page: number }> = ({ page }) => {
    const timingConfig = useGameTimingSWRConfig()
    useSWR(
      ['/test/timing', { page }],
      async ([, query]) => {
        reads.set(query.page, (reads.get(query.page) ?? 0) + 1)
        throw { response: { status: 503 } }
      },
      timingConfig
    )
    return null
  }
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(Probe, { page: 1 })))
    assert.equal(reads.get(1), 1)

    await act(async () => root.render(createElement(Probe, { page: 2 })))
    assert.equal(reads.get(2), 1)

    await act(async () => context.mock.timers.tick(GAME_TIMING_REFRESH_MS))
    assert.equal(reads.get(1), 1)
    assert.equal(reads.get(2), 2)
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('a poller publishes one final snapshot when live polling stops', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/scoreboard' })
  const restoreDom = installTestDom(browser)
  const { useRevalidateWhenPollingStops } = await import('../hooks/useGame')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  let finalReads = 0
  const revalidate = () => {
    finalReads += 1
  }
  const Probe: FC<{ polling: boolean }> = ({ polling }) => {
    useRevalidateWhenPollingStops(polling, revalidate)
    return null
  }
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(Probe, { polling: true })))
    assert.equal(finalReads, 0)
    await act(async () => root.render(createElement(Probe, { polling: false })))
    assert.equal(finalReads, 1)
    await act(async () => root.render(createElement(Probe, { polling: false })))
    assert.equal(finalReads, 1)
    await act(async () => root.render(createElement(Probe, { polling: true })))
    await act(async () => root.render(createElement(Probe, { polling: false })))
    assert.equal(finalReads, 2)
  } finally {
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
