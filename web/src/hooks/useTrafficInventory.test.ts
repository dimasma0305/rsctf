import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, type FC } from 'react'
import { installTestDom } from '../test/installDom'
import { useTrafficInventory, type TrafficInventoryReader } from './useTrafficInventory'

interface Row {
  id: string
}

const rowKey = (row: Row) => row.id

test('traffic pages abort stale navigation and append only the requested cursor page', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/1/monitor/traffic' })
  const restoreDom = installTestDom(browser)
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const reads: Array<{ path: string; cursor: string | null }> = []
  let aborted = 0
  let loadMore: (() => Promise<void>) | undefined

  const reader: TrafficInventoryReader<Row> = (path, cursor, signal) => {
    reads.push({ path, cursor })
    if (path === '/scope/a') {
      return new Promise((_resolve, reject) => {
        signal.addEventListener(
          'abort',
          () => {
            aborted += 1
            reject({ name: 'AbortError' })
          },
          { once: true }
        )
      })
    }
    return Promise.resolve(
      cursor === null ? { items: [{ id: 'b' }], nextCursor: 'older-b' } : { items: [{ id: 'c' }], nextCursor: null }
    )
  }

  const Probe: FC<{ path: string | null }> = ({ path }) => {
    const page = useTrafficInventory(path, rowKey, reader)
    loadMore = page.loadMore
    return createElement('output', null, page.items.map((row) => row.id).join(','))
  }
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(Probe, { path: null })))
    assert.equal(reads.length, 0)

    await act(async () => root.render(createElement(Probe, { path: '/scope/a' })))
    assert.deepEqual(reads, [{ path: '/scope/a', cursor: null }])

    await act(async () => root.render(createElement(Probe, { path: '/scope/b' })))
    assert.equal(aborted, 1)
    assert.equal(container.textContent, 'b')
    assert.deepEqual(reads.at(-1), { path: '/scope/b', cursor: null })

    await act(async () => loadMore?.())
    assert.equal(container.textContent, 'b,c')
    assert.deepEqual(reads.at(-1), { path: '/scope/b', cursor: 'older-b' })

    const readCount = reads.length
    await act(async () => root.render(createElement(Probe, { path: null })))
    assert.equal(reads.length, readCount)
  } finally {
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
