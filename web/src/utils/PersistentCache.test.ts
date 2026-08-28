import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { installTestDom } from '../test/installDom'
import { localCacheProvider, MAX_SWR_CACHE_ENTRIES } from './Cache'

test('SWR responses remain memory-only and legacy private snapshots are purged', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/17' })
  const restoreDom = installTestDom(browser)
  const previousIndexedDB = Object.getOwnPropertyDescriptor(globalThis, 'indexedDB')
  const previousLocalStorage = Object.getOwnPropertyDescriptor(globalThis, 'localStorage')
  let deleteDatabaseCalls = 0
  const indexedDB = {
    deleteDatabase(name: string) {
      assert.equal(name, 'rsctf-cache')
      deleteDatabaseCalls += 1
      return {} as IDBOpenDBRequest
    },
    open() {
      assert.fail('the memory-only cache must never hydrate or persist IndexedDB state')
    },
  } as unknown as IDBFactory
  Object.defineProperty(browser, 'indexedDB', { configurable: true, value: indexedDB })
  Object.defineProperties(globalThis, {
    indexedDB: { configurable: true, value: indexedDB },
    localStorage: { configurable: true, value: browser.localStorage },
  })
  browser.localStorage.setItem('rsctf-cache', 'compressed-private-response')

  try {
    const cache = localCacheProvider()
    cache.set('/api/account/profile', { data: { userId: 'private-user' } })
    cache.set('/api/game/17/ad/koth/capability', { data: { token: 'private-no-store-token' } })

    assert.equal(browser.localStorage.getItem('rsctf-cache'), null)
    assert.equal(deleteDatabaseCalls, 1)
    browser.dispatchEvent(new browser.Event('beforeunload'))
    browser.document.dispatchEvent(new browser.Event('visibilitychange'))
    assert.equal(browser.localStorage.length, 0, 'unload must not create a browser-storage snapshot')
  } finally {
    await browser.happyDOM.close()
    restoreDom()
    if (previousIndexedDB) Object.defineProperty(globalThis, 'indexedDB', previousIndexedDB)
    else delete (globalThis as typeof globalThis & { indexedDB?: IDBFactory }).indexedDB
    if (previousLocalStorage) Object.defineProperty(globalThis, 'localStorage', previousLocalStorage)
    else delete (globalThis as typeof globalThis & { localStorage?: Storage }).localStorage
  }
})

test('the memory cache rejects oversized entries and remains count bounded', () => {
  const cache = localCacheProvider()
  for (const key of cache.keys()) cache.delete(key)

  cache.set('oversized', { data: 'x'.repeat(140 * 1024) })
  assert.equal(cache.has('oversized'), false)

  for (let index = 0; index < MAX_SWR_CACHE_ENTRIES + 100; index += 1) {
    cache.set(`search:${index}`, { data: `result-${index}` })
  }
  assert.equal(Array.from(cache.keys()).length, MAX_SWR_CACHE_ENTRIES)
  assert.equal(cache.has('search:0'), false, 'old search history is evicted')
  assert.equal(cache.has(`search:${MAX_SWR_CACHE_ENTRIES + 99}`), true)
})
