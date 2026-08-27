import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, type FC } from 'react'
import { installTestDom } from '../test/installDom'
import { createConditionalScoreboardReader, isConditionalScoreboardPath } from './ConditionalScoreboard'

test('scoreboard paths are narrowly scoped to public standard and KotH boards', () => {
  assert.equal(isConditionalScoreboardPath('/api/game/7/scoreboard'), true)
  assert.equal(isConditionalScoreboardPath('/api/game/7/ad/koth/scoreboard'), true)
  assert.equal(isConditionalScoreboardPath('/api/Game/7/Ad/Scoreboard'), false)
  assert.equal(isConditionalScoreboardPath('/api/game/7/details'), false)
  assert.equal(isConditionalScoreboardPath('/api/game/7/scoreboard?monitor=true'), false)
})

test('a 304 reuses the exact parsed board and keeps validator metadata bounded', async () => {
  const requests: Array<{ path: string; etag?: string }> = []
  let decodedBodies = 0
  const reader = createConditionalScoreboardReader(async (path, etag) => {
    requests.push({ path, etag })
    if (etag) return { status: 304, data: null, etag }
    decodedBodies += 1
    const data = { path, teams: [{ id: path }] }
    return { status: 200, data, etag: `W/"${path}"` }
  }, 2)

  const first = await reader.read('/api/game/1/scoreboard')
  const unchanged = await reader.read('/api/game/1/scoreboard')
  assert.strictEqual(unchanged, first)
  assert.equal(decodedBodies, 1)
  assert.equal(requests[1].etag, 'W/"/api/game/1/scoreboard"')

  await reader.read('/api/game/2/scoreboard')
  await reader.read('/api/game/3/ad/koth/scoreboard')
  assert.equal(reader.validatorCount(), 2)
  await reader.read('/api/game/1/scoreboard')
  assert.equal(requests.at(-1)?.etag, undefined, 'the oldest validator must be evicted')
  assert.equal(decodedBodies, 4)
})

test('a delayed response cannot replace a newer retained board', async () => {
  let requests = 0
  let releaseDelayed: (() => void) | undefined
  const delayed = new Promise<void>((resolve) => {
    releaseDelayed = resolve
  })
  const reader = createConditionalScoreboardReader(async (_path, etag) => {
    requests += 1
    if (requests === 1) return { status: 200, data: { score: 1 }, etag: 'W/"v1"' }
    if (requests === 2) {
      await delayed
      return { status: 200, data: { score: 2 }, etag: 'W/"v2"' }
    }
    if (requests === 3) return { status: 200, data: { score: 3 }, etag: 'W/"v3"' }
    return { status: 304, data: null, etag }
  })

  await reader.read('/api/game/1/scoreboard')
  const older = reader.read<{ score: number }>('/api/game/1/scoreboard')
  await Promise.resolve()
  const newest = await reader.read<{ score: number }>('/api/game/1/scoreboard')
  releaseDelayed?.()
  const fenced = await older
  const unchanged = await reader.read<{ score: number }>('/api/game/1/scoreboard')

  assert.equal(newest.score, 3)
  assert.strictEqual(fenced, newest)
  assert.strictEqual(unchanged, newest)
})

test('a browser-normalized 200 with the same validator skips JSON decoding', async () => {
  let requests = 0
  const reader = createConditionalScoreboardReader(async () => {
    requests += 1
    return requests === 1
      ? { status: 200, data: '{"score":7}', etag: 'W/"stable"' }
      : { status: 200, data: 'this duplicate body must not be decoded', etag: 'W/"stable"' }
  })

  const first = await reader.read('/api/game/1/scoreboard')
  const unchanged = await reader.read('/api/game/1/scoreboard')
  assert.deepEqual(first, { score: 7 })
  assert.strictEqual(unchanged, first)
})

test('a retained validator cannot bypass an account-view denial or changed view', async () => {
  let view: 'public' | 'denied' | 'monitor' = 'public'
  const sentValidators: Array<string | undefined> = []
  const reader = createConditionalScoreboardReader(async (_path, etag) => {
    sentValidators.push(etag)
    if (view === 'denied') return { status: 403, data: null }
    const currentEtag = `W/"${view}"`
    return etag === currentEtag
      ? { status: 304, data: null, etag: currentEtag }
      : { status: 200, data: JSON.stringify({ view }), etag: currentEtag }
  })

  const publicBoard = await reader.read<{ view: string }>('/api/game/7/ad/koth/scoreboard')
  view = 'denied'
  await assert.rejects(() => reader.read('/api/game/7/ad/koth/scoreboard'), /unexpected.*403/)
  view = 'monitor'
  const monitorBoard = await reader.read<{ view: string }>('/api/game/7/ad/koth/scoreboard')

  assert.equal(publicBoard.view, 'public')
  assert.equal(monitorBoard.view, 'monitor')
  assert.notStrictEqual(monitorBoard, publicBoard)
  assert.equal(sentValidators[1], 'W/"public"')
  assert.equal(sentValidators[2], 'W/"public"')
})

test('SWR does not render an unchanged maximum-board reference after a 304', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/17/scoreboard' })
  const restoreDom = installTestDom(browser)
  const { default: useSWR, SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const board = {
    updateTimeUtc: 1,
    items: Array.from({ length: 500 }, (_, id) => ({ id, score: id })),
  }
  let requests = 0
  const reader = createConditionalScoreboardReader(async (_path, etag) => {
    requests += 1
    return etag ? { status: 304, data: null, etag } : { status: 200, data: board, etag: 'W/"stable-board"' }
  })
  let renders = 0
  let revalidate: (() => Promise<unknown>) | undefined
  const Probe: FC = () => {
    const { data, mutate } = useSWR('/api/game/17/scoreboard', reader.read, {
      compare: Object.is,
      dedupingInterval: 0,
    })
    renders += 1
    revalidate = () => mutate()
    return createElement('output', null, data?.items.length ?? 0)
  }
  const App: FC = () => createElement(SWRConfig, { value: { provider: () => new Map() } }, createElement(Probe))
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(App)))
    assert.equal(container.textContent, '500')
    const settledRenders = renders
    await act(async () => {
      await revalidate?.()
    })
    assert.equal(requests, 2)
    assert.equal(renders, settledRenders)
  } finally {
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
