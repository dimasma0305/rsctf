import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, type FC } from 'react'
import type { BareFetcher, SWRConfiguration } from 'swr'
import type { DetailedGameInfoModel } from '../Api'
import { ParticipationStatus } from '../Api'
import { installTestDom } from '../test/installDom'

type Deferred<T> = {
  promise: Promise<T>
  resolve: (value: T) => void
}

const deferred = <T>(): Deferred<T> => {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((accept) => {
    resolve = accept
  })
  return { promise, resolve }
}

const game = (
  id: number,
  title: string,
  start: number,
  end: number,
  status: ParticipationStatus
): DetailedGameInfoModel => ({ id, title, start, end, status, practiceMode: false })

test('cached game paint waits for this key before applying retained schedule and participation changes', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/41/challenges' })
  const restoreDom = installTestDom(browser)
  const now = Date.now()
  const cached = game(41, 'cached future event', now + 600_000, now + 1_200_000, ParticipationStatus.Unsubmitted)
  const firstRead = deferred<DetailedGameInfoModel>()
  const laterReads: Deferred<DetailedGameInfoModel>[] = []
  const fetcher: BareFetcher<DetailedGameInfoModel> = () => {
    if (laterReads.length === 0) {
      laterReads.push(firstRead)
      return firstRead.promise
    }
    const read = deferred<DetailedGameInfoModel>()
    laterReads.push(read)
    return read.promise
  }
  const cache = new Map()
  const swrConfig: SWRConfiguration = {
    provider: () => cache,
    fallback: { '/api/game/41': cached },
    fetcher,
    keepPreviousData: true,
    dedupingInterval: 0,
  }
  const { SWRConfig } = await import('swr')
  const { useGameAccess, useGameStatus } = await import('../hooks/useGame')
  const { observeServerTime, serverClockTestApi, useServerClockReady } = await import('./ServerClock')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  let refresh: (() => Promise<unknown>) | undefined
  const Probe: FC = () => {
    const { game: current, liveReadReady, mutate, status } = useGameAccess(41)
    const clockReady = useServerClockReady()
    const { started } = useGameStatus(current)
    refresh = () => mutate()

    let access = 'waiting'
    if (current && clockReady && liveReadReady) {
      access = !started
        ? 'not-started'
        : status === ParticipationStatus.Suspended
          ? 'suspended'
          : status === ParticipationStatus.Accepted
            ? 'allowed'
            : 'not-joined'
    }
    return createElement('output', null, `${current?.title ?? 'none'}:${access}`)
  }
  const App: FC = () => createElement(SWRConfig, { value: swrConfig }, createElement(Probe))
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    serverClockTestApi.reset()
    assert.equal(observeServerTime(now, now), true)
    await act(async () => root.render(createElement(App)))

    assert.equal(laterReads.length, 1)
    assert.equal(container.textContent, 'cached future event:waiting')

    await act(async () => {
      firstRead.resolve(game(41, 'live active event', now - 60_000, now + 600_000, ParticipationStatus.Accepted))
      await firstRead.promise
    })
    assert.equal(container.textContent, 'live active event:allowed')

    await act(async () => {
      void refresh?.()
      await Promise.resolve()
    })
    assert.equal(laterReads.length, 2)
    assert.equal(container.textContent, 'live active event:allowed')
    await act(async () => {
      laterReads[1].resolve(
        game(41, 'participation changed', now - 60_000, now + 600_000, ParticipationStatus.Suspended)
      )
      await laterReads[1].promise
    })
    assert.equal(container.textContent, 'participation changed:suspended')

    await act(async () => {
      void refresh?.()
      await Promise.resolve()
    })
    assert.equal(laterReads.length, 3)
    await act(async () => {
      laterReads[2].resolve(game(41, 'schedule changed', now + 600_000, now + 1_200_000, ParticipationStatus.Accepted))
      await laterReads[2].promise
    })
    assert.equal(container.textContent, 'schedule changed:not-started')
  } finally {
    await act(async () => root.unmount())
    serverClockTestApi.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('a stale game A response cannot make retained game B access-ready', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/51/challenges' })
  const restoreDom = installTestDom(browser)
  const now = Date.now()
  const reads = new Map<string, Deferred<DetailedGameInfoModel>>()
  const fetcher: BareFetcher<DetailedGameInfoModel> = (request) => {
    const key = typeof request === 'string' ? request : String(request)
    const read = deferred<DetailedGameInfoModel>()
    reads.set(key, read)
    return read.promise
  }
  const swrConfig: SWRConfiguration = {
    provider: () => new Map(),
    fallback: {
      '/api/game/51': game(51, 'cached A', now - 60_000, now + 600_000, ParticipationStatus.Accepted),
      '/api/game/52': game(52, 'cached B', now - 60_000, now + 600_000, ParticipationStatus.Accepted),
    },
    fetcher,
    keepPreviousData: true,
    dedupingInterval: 0,
  }
  const { SWRConfig } = await import('swr')
  const { useGameAccess } = await import('../hooks/useGame')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const Probe: FC<{ id: number }> = ({ id }) => {
    const { game: current, liveReadReady } = useGameAccess(id)
    return createElement('output', null, `${id}:${current?.id ?? 'none'}:${liveReadReady ? 'ready' : 'waiting'}`)
  }
  const App: FC<{ id: number }> = ({ id }) =>
    createElement(SWRConfig, { value: swrConfig }, createElement(Probe, { id }))
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(App, { id: 51 })))
    assert.ok(reads.has('/api/game/51'))
    assert.equal(container.textContent, '51:51:waiting')

    await act(async () => root.render(createElement(App, { id: 52 })))
    assert.ok(reads.has('/api/game/52'))
    assert.match(container.textContent ?? '', /^52:(51|52):waiting$/)

    const staleA = reads.get('/api/game/51')
    assert.ok(staleA)
    await act(async () => {
      staleA.resolve(game(51, 'late A', now - 60_000, now + 600_000, ParticipationStatus.Accepted))
      await staleA.promise
    })
    assert.match(container.textContent ?? '', /^52:(51|52):waiting$/)

    const currentB = reads.get('/api/game/52')
    assert.ok(currentB)
    await act(async () => {
      currentB.resolve(game(52, 'live B', now - 60_000, now + 600_000, ParticipationStatus.Accepted))
      await currentB.promise
    })
    assert.equal(container.textContent, '52:52:ready')
  } finally {
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
