import axios, { AxiosHeaders, type AxiosResponse, type InternalAxiosRequestConfig } from 'axios'
import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, type FC } from 'react'
import { installTestDom } from '../test/installDom'

const clockResponse = (
  config: InternalAxiosRequestConfig,
  serverTime: number,
  finalUrl: string | null,
  adapter: 'browser' | 'node' = 'browser'
): AxiosResponse => ({
  data: { serverTime },
  status: 200,
  statusText: 'OK',
  headers: new AxiosHeaders(),
  config,
  request:
    finalUrl === null ? {} : adapter === 'browser' ? { responseURL: finalUrl } : { res: { responseUrl: finalUrl } },
})

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
    assert.equal(observeServerTime(serverStart, localStart, localStart, 2), true)
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
    assert.equal(observeServerTime(serverStart - 1, localStart + 3_000, localStart - 1_000, 1), false)
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

test('shared Axios clock ignores external proof responses and redirected API responses', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/19/challenges' })
  const restoreDom = installTestDom(browser)
  const localNow = 2_000_020_000_000
  context.mock.timers.enable({ apis: ['Date'], now: new Date(localNow) })
  const { hasLiveServerClockSample, installServerClock, serverClockTestApi } = await import('./ServerClock')
  const client = axios.create()

  try {
    serverClockTestApi.reset()
    installServerClock(client)

    // Event VPN proof calls intentionally share the generated Axios instance.
    // Their provider controls this JSON and must never become a clock authority.
    await client.post('https://event-vpn.test/proof', undefined, {
      adapter: async (config) => ({
        ...clockResponse(config, localNow + 24 * 60 * 60_000, 'https://event-vpn.test/proof'),
        data: {
          proof: 'provider-controlled-proof',
          proofHeader: 'X-RSCTF-VPN-Proof',
          expiresAtUtc: localNow + 60_000,
          serverTime: localNow + 24 * 60 * 60_000,
        },
      }),
    })
    assert.equal(hasLiveServerClockSample(), false)

    // Same-origin pages outside the RSCTF API contract are not authoritative.
    await client.get('/event-vpn/proof', {
      adapter: async (config) =>
        clockResponse(config, localNow + 12 * 60 * 60_000, 'https://rsctf.test/event-vpn/proof'),
    })
    assert.equal(hasLiveServerClockSample(), false)

    // Both the configured URL and the redirect-resolved response URL must be
    // canonical. This rejects redirects away from the trusted API origin and
    // adapters that cannot prove where the response ultimately came from.
    await client.get('/api/game/19', {
      adapter: async (config) => clockResponse(config, localNow + 8 * 60 * 60_000, 'https://event-vpn.test/proof'),
    })
    await client.get('/api/game/19', {
      adapter: async (config) => clockResponse(config, localNow + 8 * 60 * 60_000, null),
    })
    await client.get('https://event-vpn.test/api/proof', {
      adapter: async (config) => clockResponse(config, localNow + 8 * 60 * 60_000, 'https://rsctf.test/api/proof'),
    })
    assert.equal(hasLiveServerClockSample(), false)

    await client.get('/api/game/19', {
      adapter: async (config) => clockResponse(config, localNow + 1_000, 'https://rsctf.test/api/game/19'),
    })
    assert.equal(hasLiveServerClockSample(), true)
    assert.equal(serverClockTestApi.offset(), 1_000)

    // Axios baseURL joining remains supported for canonical API clients.
    await client.get('game/20', {
      baseURL: 'https://rsctf.test/api',
      adapter: async (config) => clockResponse(config, localNow + 2_000, 'https://rsctf.test/api/game/20'),
    })
    assert.equal(serverClockTestApi.offset(), 2_000)

    // A stale canonical response still cannot overwrite a newer canonical
    // sample after the trust check has run.
    let releaseOlder: (() => void) | undefined
    let markOlderStarted: (() => void) | undefined
    const olderStarted = new Promise<void>((resolve) => {
      markOlderStarted = resolve
    })
    const older = client.get('/api/game/21', {
      adapter: async (config) => {
        markOlderStarted?.()
        await new Promise<void>((resolve) => {
          releaseOlder = resolve
        })
        return clockResponse(config, localNow + 30_000, 'https://rsctf.test/api/game/21')
      },
    })
    await olderStarted
    await client.get('https://rsctf.test/api/game/22', {
      adapter: async (config) => clockResponse(config, localNow + 3_000, 'https://rsctf.test/api/game/22'),
    })
    assert.equal(serverClockTestApi.offset(), 3_000)
    assert.ok(releaseOlder)
    releaseOlder()
    await older
    assert.equal(serverClockTestApi.offset(), 3_000)
  } finally {
    serverClockTestApi.reset()
    context.mock.timers.reset()
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('non-browser clock authority requires an absolute API base URL', async (context) => {
  const localNow = 2_000_030_000_000
  context.mock.timers.enable({ apis: ['Date'], now: new Date(localNow) })
  const { hasLiveServerClockSample, installServerClock, serverClockTestApi } = await import('./ServerClock')

  try {
    serverClockTestApi.reset()
    const ambiguousClient = axios.create()
    installServerClock(ambiguousClient)
    await ambiguousClient.get('/api/game/31', {
      adapter: async (config) => clockResponse(config, localNow + 30_000, 'https://rsctf.test/api/game/31', 'node'),
    })
    assert.equal(hasLiveServerClockSample(), false)

    const canonicalClient = axios.create({ baseURL: 'https://rsctf.test/api' })
    installServerClock(canonicalClient)
    await canonicalClient.get('game/31', {
      adapter: async (config) => clockResponse(config, localNow + 500, 'https://rsctf.test/api/game/31', 'node'),
    })
    assert.equal(hasLiveServerClockSample(), true)
    assert.equal(serverClockTestApi.offset(), 500)
  } finally {
    serverClockTestApi.reset()
    context.mock.timers.reset()
  }
})

test('newer cross-replica samples correct backward while stale responses stay fenced', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/challenges' })
  const restoreDom = installTestDom(browser)
  const localStart = 2_000_010_000_000
  context.mock.timers.enable({
    apis: ['Date', 'setInterval', 'setTimeout'],
    now: new Date(localStart),
  })
  const { observeServerTime, serverClockTestApi, useServerClockOffset } = await import('./ServerClock')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const Probe: FC = () => {
    const offset = useServerClockOffset()
    return createElement('output', null, offset === null ? 'waiting' : offset)
  }
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    serverClockTestApi.reset()
    await act(async () => root.render(createElement(Probe)))
    assert.equal(container.textContent, 'waiting')

    // Request 1 remains in flight while request 2 reaches a replica whose
    // wall clock is two minutes fast.
    await act(async () => context.mock.timers.tick(10))
    await act(async () => {
      assert.equal(observeServerTime(localStart + 120_010, Date.now(), localStart + 5, 2), true)
    })
    assert.equal(serverClockTestApi.offset(), 120_000)
    assert.equal(container.textContent, '120000')

    // A newer, lower-latency response from a synchronized replica has a lower
    // absolute serverTime. Request ordering must let it correct the offset.
    await act(async () => context.mock.timers.tick(10))
    await act(async () => {
      assert.equal(observeServerTime(localStart + 20, Date.now(), localStart + 19, 3), true)
    })
    assert.equal(serverClockTestApi.offset(), 0)
    assert.equal(container.textContent, '0')

    // The first request finally returns from the fast replica. Even its higher
    // absolute serverTime cannot overwrite the newer response.
    await act(async () => context.mock.timers.tick(10))
    assert.equal(observeServerTime(localStart + 120_030, Date.now(), localStart, 1), false)
    assert.equal(serverClockTestApi.offset(), 0)
    assert.equal(container.textContent, '0')

    // A later authoritative response also recovers when time synchronization
    // steps the server clock backward instead of waiting for wall time to catch up.
    await act(async () => context.mock.timers.tick(10))
    await act(async () => {
      assert.equal(observeServerTime(Date.now() - 60_000, Date.now(), Date.now(), 4), true)
    })
    assert.equal(serverClockTestApi.offset(), -60_000)
    assert.equal(container.textContent, '-60000')
  } finally {
    await act(async () => root.unmount())
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
