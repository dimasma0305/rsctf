import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, type FC } from 'react'
import { SWRConfig } from 'swr'
import type { CheatIncidentPageItem } from '@Api'
import { installTestDom } from '../test/installDom'
import { type CheatIncidentPageQuery, type CheatIncidentPageReader, useCheatIncidentFeed } from './useCheatIncidentFeed'

const incident = (id: number, observedAt: number) => ({ id, observedAt }) as CheatIncidentPageItem

test('the incident feed is silent while inactive and reconciles delta and older cursors without duplicates', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/7/monitor/cheat' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 0 })
  context.mock.method(Math, 'random', () => 0.5)
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const queries: CheatIncidentPageQuery[] = []
  let loadOlder: (() => Promise<void>) | undefined

  const reader: CheatIncidentPageReader = async (_gameId, query) => {
    queries.push(query)
    if (query.beforeId !== undefined) {
      return { data: [incident(5, 500)], nextBefore: null, checkpointId: 10, hasMore: false }
    }
    if (query.afterId !== undefined) {
      return { data: [incident(11, 1_100)], nextBefore: null, checkpointId: 11, hasMore: false }
    }
    return {
      data: [incident(10, 1_000)],
      nextBefore: { observedAt: 1_000, id: 10 },
      checkpointId: 10,
      hasMore: true,
    }
  }

  const Probe: FC<{ active: boolean }> = ({ active }) => {
    const feed = useCheatIncidentFeed(7, active, reader)
    loadOlder = feed.loadOlder
    return createElement('output', null, feed.data.map((row) => row.id).join(','))
  }
  const cache = new Map()
  const Scope: FC<{ active: boolean }> = ({ active }) =>
    createElement(
      SWRConfig,
      { value: { provider: () => cache, dedupingInterval: 0, isVisible: () => true, isOnline: () => true } },
      createElement(Probe, { active })
    )
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(Scope, { active: false })))
    await act(async () => context.mock.timers.tick(10 * 60_000))
    assert.equal(queries.length, 0, 'an inactive tab must not issue an initial or interval request')

    await act(async () => root.render(createElement(Scope, { active: true })))
    assert.equal(container.textContent, '10')
    assert.deepEqual(queries[0], { limit: 100 })

    await act(async () => loadOlder?.())
    assert.equal(container.textContent, '10,5')
    assert.deepEqual(queries[1], { limit: 100, beforeObservedAt: 1_000, beforeId: 10 })

    await act(async () => context.mock.timers.tick(10_000))
    assert.equal(container.textContent, '11,10,5')
    assert.deepEqual(queries[2], { limit: 100, afterId: 10 })

    const requestCount = queries.length
    await act(async () => root.render(createElement(Scope, { active: false })))
    await act(async () => context.mock.timers.tick(10 * 60_000))
    assert.equal(queries.length, requestCount)
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
