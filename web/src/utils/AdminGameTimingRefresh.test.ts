import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { act, createElement, type FC } from 'react'
import { installTestDom } from '../test/installDom'

test('admin games use one paginated timing subscription instead of a one-shot request', () => {
  const source = readFileSync('src/pages/admin/games/Index.tsx', 'utf8')

  assert.equal((source.match(/useEditGetGames/g) ?? []).length, 1)
  assert.match(source, /api\.edit\.useEditGetGames\([\s\S]*?timingConfig/)
  assert.doesNotMatch(source, /api\.edit\.editGetGames/)
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
