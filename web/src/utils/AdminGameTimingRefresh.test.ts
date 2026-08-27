import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { act, createElement, type FC } from 'react'
import type { BareFetcher } from 'swr'
import type { ArrayResponseOfGameInfoModel } from '../Api'
import { installTestDom } from '../test/installDom'

type Deferred<T> = {
  promise: Promise<T>
  reject: (reason: unknown) => void
  resolve: (value: T) => void
}

const deferred = <T>(): Deferred<T> => {
  let reject!: (reason: unknown) => void
  let resolve!: (value: T) => void
  const promise = new Promise<T>((accept, decline) => {
    resolve = accept
    reject = decline
  })
  return { promise, reject, resolve }
}

const gamePage = (page: number): ArrayResponseOfGameInfoModel => ({
  data: [
    {
      id: page * 100 + 1,
      title: `Page ${page} game`,
      start: 0,
      end: 600_000,
    },
  ],
  length: 1,
  total: 30,
})

test('admin games use one paginated timing subscription instead of a one-shot request', () => {
  const source = readFileSync('src/pages/admin/games/Index.tsx', 'utf8')

  assert.equal((source.match(/useEditGetGames/g) ?? []).length, 1)
  assert.match(source, /api\.edit\.useEditGetGames\([\s\S]*?\.\.\.timingConfig[\s\S]*?keepPreviousData:\s*false/)
  assert.doesNotMatch(source, /api\.edit\.editGetGames/)
})

test('admin games remove prior-page actions while the next page is slow or fails', async () => {
  const browser = new Window({ url: 'https://rsctf.test/admin/games' })
  const restoreDom = installTestDom(browser)
  const secondPage = deferred<ArrayResponseOfGameInfoModel>()
  const reads: number[] = []
  const invoked: number[] = []
  const fetcher: BareFetcher<ArrayResponseOfGameInfoModel> = async (request) => {
    assert.ok(Array.isArray(request))
    assert.equal(request[0], '/api/edit/games')
    const query = request[1] as { count: number; skip: number }
    const requestedPage = query.skip / query.count + 1
    reads.push(requestedPage)
    return requestedPage === 1 ? gamePage(1) : secondPage.promise
  }
  const { useGameTimingSWRConfig } = await import('../hooks/useGame')
  const { default: api } = await import('../Api')
  const { SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const cache = new Map()

  const Probe: FC<{ page: number }> = ({ page }) => {
    const timingConfig = useGameTimingSWRConfig()
    const { data, error } = api.edit.useEditGetGames(
      { count: 15, skip: (page - 1) * 15 },
      { ...timingConfig, keepPreviousData: false }
    )

    return createElement(
      'section',
      null,
      createElement('output', { id: 'page-context' }, `page-${page}`),
      error ? createElement('output', { id: 'page-error' }, 'failed') : null,
      data?.data.map((game) =>
        createElement(
          'button',
          {
            key: game.id,
            type: 'button',
            'data-game-id': game.id,
            onClick: () => game.id !== undefined && invoked.push(game.id),
          },
          `Edit ${game.title}`
        )
      )
    )
  }
  const Scope: FC<{ page: number }> = ({ page }) =>
    createElement(
      SWRConfig,
      {
        value: {
          provider: () => cache,
          fetcher,
          keepPreviousData: true,
          dedupingInterval: 0,
        },
      },
      createElement(Probe, { page })
    )
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(Scope, { page: 1 })))
    assert.deepEqual(reads, [1])
    const oldAction = container.querySelector<HTMLButtonElement>('[data-game-id="101"]')
    assert.ok(oldAction)
    oldAction.click()
    assert.deepEqual(invoked, [101])

    await act(async () => root.render(createElement(Scope, { page: 2 })))
    assert.deepEqual(reads, [1, 2])
    assert.equal(container.querySelector('#page-context')?.textContent, 'page-2')
    assert.equal(container.querySelectorAll('[data-game-id]').length, 0)
    assert.equal(oldAction.isConnected, false)
    oldAction.click()
    assert.deepEqual(invoked, [101], 'a detached prior-page action cannot run in the page-2 context')

    await act(async () => {
      secondPage.reject({ response: { status: 404 } })
      await secondPage.promise.catch(() => undefined)
    })
    assert.equal(container.querySelector('#page-error')?.textContent, 'failed')
    assert.equal(container.querySelectorAll('[data-game-id]').length, 0)
  } finally {
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('admin game timing refresh adopts another organizer update with one request per page', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/admin/games' })
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
  const { default: api } = await import('../Api')
  const { SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const cache = new Map()
  const reads = new Map<number, number>()
  let remoteEnd = GAME_TIMING_REFRESH_MS * 10

  const Probe: FC<{ page: number }> = ({ page }) => {
    const timingConfig = useGameTimingSWRConfig()
    const { data } = api.edit.useEditGetGames({ count: 15, skip: (page - 1) * 15 }, timingConfig)
    return createElement('output', null, data?.data.find((game) => game.id === 7)?.end ?? 'loading')
  }
  const Scope: FC<{ page: number }> = ({ page }) =>
    createElement(
      SWRConfig,
      {
        value: {
          provider: () => cache,
          isVisible: () => browser.document.visibilityState !== 'hidden',
          isOnline: () => browser.navigator.onLine,
          fetcher: async (request: unknown) => {
            assert.ok(Array.isArray(request))
            assert.equal(request[0], '/api/edit/games')
            const query = request[1] as { count: number; skip: number }
            assert.equal(query.count, 15)
            const requestedPage = query.skip / query.count + 1
            reads.set(requestedPage, (reads.get(requestedPage) ?? 0) + 1)
            return {
              data: Array.from({ length: 15 }, (_, index) => ({
                id: index + 1,
                title: `Game ${index + 1}`,
                start: 0,
                end: index === 6 ? remoteEnd : GAME_TIMING_REFRESH_MS * 10,
              })),
              length: 15,
              total: 30,
            }
          },
        },
      },
      createElement(Probe, { page })
    )
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(Scope, { page: 1 })))
    assert.equal(reads.get(1), 1)
    assert.equal(container.textContent, `${GAME_TIMING_REFRESH_MS * 10}`)

    // Another organizer changes the schedule without touching this browser.
    // The single page subscription, rather than each of its 15 cards, owns the
    // next read and publishes that newer window.
    remoteEnd = GAME_TIMING_REFRESH_MS * 20
    await act(async () => context.mock.timers.tick(GAME_TIMING_REFRESH_MS))
    assert.equal(reads.get(1), 2)
    assert.equal(container.textContent, `${GAME_TIMING_REFRESH_MS * 20}`)

    visibilityState = 'hidden'
    await act(async () => context.mock.timers.tick(GAME_TIMING_REFRESH_MS * 3))
    assert.equal(reads.get(1), 2)

    visibilityState = 'visible'
    online = false
    await act(async () => context.mock.timers.tick(GAME_TIMING_REFRESH_MS))
    assert.equal(reads.get(1), 2)

    online = true
    await act(async () => context.mock.timers.tick(GAME_TIMING_REFRESH_MS))
    assert.equal(reads.get(1), 3)

    await act(async () => root.render(createElement(Scope, { page: 2 })))
    assert.equal(reads.get(2), 1)
    const firstPageReads = reads.get(1)
    await act(async () => context.mock.timers.tick(GAME_TIMING_REFRESH_MS))
    assert.equal(reads.get(1), firstPageReads)
    assert.equal(reads.get(2), 2)
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
