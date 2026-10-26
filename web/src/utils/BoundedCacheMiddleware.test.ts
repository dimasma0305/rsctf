import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import useSWR, { SWRConfig } from 'swr'
import { installTestDom } from '../test/installDom'
import { boundedCacheMiddleware } from './BoundedCacheMiddleware'
import { BoundedMemoryCache } from './Cache'

test('cache budgets preserve mounted SWR responses and evict them after their last consumer leaves', async () => {
  const browser = new Window({ url: 'https://rsctf.test/' })
  const restore = installTestDom(browser)
  const { createRoot } = await import('react-dom/client')
  const cache = new BoundedMemoryCache(1, 1024)
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const config = {
    provider: () => cache,
    use: [boundedCacheMiddleware],
    fetcher: async (key: string) => ({ key, text: 'x'.repeat(4096) }),
    revalidateOnFocus: false,
    revalidateOnReconnect: false,
    shouldRetryOnError: false,
    dedupingInterval: 0,
  }
  const Probe = ({ id }: { id: string }) => {
    const { data } = useSWR<{ key: string; text: string }>(id)
    return createElement('output', null, data ? `${data.key}:${data.text.length}` : 'loading')
  }
  const App = ({ ids }: { ids: string[] }) =>
    createElement(
      SWRConfig,
      { value: config },
      ids.map((id) => createElement(Probe, { id, key: id }))
    )
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  try {
    await act(async () => root.render(createElement(App, { ids: ['one', 'two'] })))
    assert.equal(container.textContent, 'one:4096two:4096')
    assert.equal(cache.size, 2, 'both active models remain visible despite a smaller budget')
    cache.set('inactive', { data: 'small' })
    assert.equal(cache.has('inactive'), false)
    await act(async () => root.render(createElement(App, { ids: ['two'] })))
    assert.equal(container.textContent, 'two:4096')
    assert.equal(cache.has('one'), false)
    await act(async () => root.unmount())
    assert.equal(cache.size, 0)
    assert.equal(cache.estimatedBytes, 0)
  } finally {
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restore()
  }
})
