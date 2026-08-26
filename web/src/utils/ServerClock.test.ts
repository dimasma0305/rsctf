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
    assert.equal(serverClockTestApi.offset(), -60 * 60 * 1000 - 100)
    assert.equal(serverClockTestApi.bestRoundTrip(), 200)

    // A newer low-latency response corrects the estimate. A later slow
    // response advances ordering but cannot degrade the offset in this window.
    assert.equal(observeServerTime(serverStart + 1_000, localStart + 1_020, localStart + 1_000), true)
    assert.equal(serverClockTestApi.offset(), -60 * 60 * 1000 - 10)
    assert.equal(serverClockTestApi.bestRoundTrip(), 20)
    assert.equal(observeServerTime(serverStart + 2_000, localStart + 4_000, localStart + 2_000), true)
    assert.equal(serverClockTestApi.offset(), -60 * 60 * 1000 - 10)
  } finally {
    serverClockTestApi.reset()
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('timing polling retries a transient failure at one bounded cadence', async (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const { GAME_TIMING_REFRESH_MS, gameTimingSWRConfig, shouldRetryGameTimingError } = await import('../hooks/useGame')
  let recoveredReads = 0

  try {
    assert.equal(shouldRetryGameTimingError({ response: { status: 503 } }), true)
    assert.equal(shouldRetryGameTimingError({ response: { status: 404 } }), false)
    gameTimingSWRConfig.onErrorRetry(
      { response: { status: 503 } },
      '/api/game/recent',
      gameTimingSWRConfig,
      () => {
        recoveredReads += 1
      },
      { retryCount: 1 }
    )
    context.mock.timers.tick(GAME_TIMING_REFRESH_MS - 1)
    assert.equal(recoveredReads, 0)
    context.mock.timers.tick(1)
    assert.equal(recoveredReads, 1)
  } finally {
    context.mock.timers.reset()
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
