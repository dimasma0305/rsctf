import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, type FC } from 'react'
import type { BareFetcher, SWRConfiguration } from 'swr'
import type { DetailedGameInfoModel } from '../Api'
import { ParticipationStatus } from '../Api'
import { installTestDom } from '../test/installDom'

const game = (id: number, title: string): DetailedGameInfoModel => ({
  id,
  title,
  start: 1,
  end: 120_000,
  status: ParticipationStatus.Accepted,
  practiceMode: false,
})

test('game landing redirects initial and terminal failures but retains loaded data for retryable failures', async () => {
  const { shouldRedirectGameLandingError } = await import('../hooks/useGame')

  assert.equal(shouldRedirectGameLandingError(undefined, false), false)
  assert.equal(shouldRedirectGameLandingError({ response: { status: 503 } }, false), true)
  assert.equal(shouldRedirectGameLandingError(new TypeError('network unavailable'), false), true)

  assert.equal(shouldRedirectGameLandingError({ response: { status: 503 } }, true), false)
  assert.equal(shouldRedirectGameLandingError(new TypeError('network unavailable'), true), false)
  assert.equal(shouldRedirectGameLandingError({ response: { status: 429 } }, true), false)

  for (const status of [400, 401, 403, 404]) {
    assert.equal(shouldRedirectGameLandingError({ response: { status } }, true), true)
  }
})

test('loaded game survives one transient timing retry and adopts the recovered response', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/73' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const { GAME_TIMING_REFRESH_MS, shouldRedirectGameLandingError, useGame } = await import('../hooks/useGame')
  const { SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  let reads = 0
  const fetcher: BareFetcher<DetailedGameInfoModel> = async (request) => {
    assert.equal(request, '/api/game/73')
    reads += 1
    if (reads === 1) return game(73, 'loaded event')
    if (reads === 2) throw { response: { status: 503 } }
    return game(73, 'recovered event')
  }
  const swrConfig: SWRConfiguration = {
    provider: () => new Map(),
    fetcher,
    dedupingInterval: 0,
  }
  let poll: (() => Promise<unknown>) | undefined
  const Probe: FC = () => {
    const { game: current, error, mutate } = useGame(73)
    poll = () => mutate().catch(() => undefined)
    const redirect = shouldRedirectGameLandingError(error, current !== undefined)
    return createElement(
      'output',
      null,
      `${current?.title ?? 'none'}:${error ? 'error' : 'ok'}:${redirect ? 'redirect' : 'stay'}`
    )
  }
  const App: FC = () => createElement(SWRConfig, { value: swrConfig }, createElement(Probe))
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(App)))
    assert.equal(reads, 1)
    assert.equal(container.textContent, 'loaded event:ok:stay')

    await act(async () => {
      await poll?.()
    })
    assert.equal(reads, 2)
    assert.equal(container.textContent, 'loaded event:error:stay')

    await act(async () => context.mock.timers.tick(GAME_TIMING_REFRESH_MS / 2 - 1))
    assert.equal(reads, 2)
    assert.equal(container.textContent, 'loaded event:error:stay')

    await act(async () => context.mock.timers.tick(GAME_TIMING_REFRESH_MS / 2 + 1))
    assert.equal(reads, 3)
    assert.equal(container.textContent, 'recovered event:ok:stay')
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
