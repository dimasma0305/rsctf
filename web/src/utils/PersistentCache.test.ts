import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { installTestDom } from '../test/installDom'
import {
  BoundedMemoryCache,
  clearLegacySensitiveBrowserStorage,
  localCacheProvider,
  retirePersistentCacheEntry,
} from './Cache'

test('blocked browser storage cannot prevent the memory-only cache from starting', () => {
  const previous = Object.getOwnPropertyDescriptor(globalThis, 'window')
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: {
      get localStorage() {
        throw new Error('Storage access denied')
      },
    },
  })
  try {
    assert.doesNotThrow(clearLegacySensitiveBrowserStorage)
  } finally {
    if (previous) Object.defineProperty(globalThis, 'window', previous)
    else Reflect.deleteProperty(globalThis, 'window')
  }
})

test('mounted consumers retain oversized working data and release its memory afterwards', () => {
  const cache = new BoundedMemoryCache(2, 1024)
  const releaseA = cache.retain('active')
  const releaseB = cache.retain('active')
  cache.set('active', { data: { text: 'x'.repeat(4096) } })
  cache.set('cold', { data: 'small' })
  assert.ok(cache.has('active'))
  assert.equal(cache.has('cold'), false)
  releaseA()
  releaseA()
  assert.ok(cache.has('active'))
  releaseB()
  assert.equal(cache.has('active'), false)
  assert.equal(cache.estimatedBytes, 0)
})

test('old consumer cleanup cannot release a new lease after clearing the cache', () => {
  const cache = new BoundedMemoryCache(2, 1024)
  const oldRelease = cache.retain('same-key')
  cache.clear()
  const newRelease = cache.retain('same-key')
  cache.set('same-key', { data: 'x'.repeat(4096) })
  oldRelease()
  assert.ok(cache.has('same-key'))
  newRelease()
  assert.equal(cache.size, 0)
})

test('filtered SWR iteration stays finite while reads update recency', () => {
  const cache = new BoundedMemoryCache()
  cache.set('a', { data: 1 })
  cache.set('b', { data: 2 })
  const visited: string[] = []
  for (const key of cache.keys()) {
    cache.get(key)
    visited.push(key)
    assert.ok(visited.length <= 2)
  }
  assert.deepEqual(visited, ['a', 'b'])
})

test('eviction fences a late background response before it can repopulate private data', () => {
  const cache = new BoundedMemoryCache(2, 256)
  cache.set('old-key', { data: 'x'.repeat(4096), isValidating: true, _k: 'old-key' })
  assert.equal(cache.has('old-key'), false)
  cache.set('old-key', { data: 'late private response', isValidating: false })
  assert.equal(cache.has('old-key'), false)
  cache.set('old-key', { data: 'new', _k: 'old-key' })
  assert.equal(cache.get('old-key')?.data, 'new')
})

test('retirement overrides active leases and byte accounting follows replacement and deletion', () => {
  const cache = new BoundedMemoryCache()
  const release = cache.retain('private-key')
  cache.set('private-key', { data: 'x'.repeat(200), isValidating: true, _k: 'private-key' })
  const before = cache.estimatedBytes
  cache.set('private-key', { data: 'small', isValidating: true, _k: 'private-key' })
  assert.ok(cache.estimatedBytes < before)
  retirePersistentCacheEntry(cache, 'private-key')
  cache.set('private-key', { data: 'old account response', isValidating: false })
  assert.equal(cache.size, 0)
  assert.equal(cache.estimatedBytes, 0)
  release()
  cache.set('other', { data: 1 })
  cache.delete('other')
  assert.equal(cache.estimatedBytes, 0)
})

test('reading a hot response protects it from later cache eviction', () => {
  const cache = localCacheProvider()
  cache.clear()
  cache.set('hot', { data: 'current profile' })
  for (let index = 0; index < 600; index += 1) {
    cache.get('hot')
    cache.set(`cold-${index}`, { data: index })
  }
  assert.equal(cache.get('hot')?.data, 'current profile')
  cache.clear()
})

test('inactive responses obey a byte budget as well as an entry limit', () => {
  const cache = localCacheProvider()
  cache.clear()
  for (let index = 0; index < 80; index += 1) {
    cache.set(`large-${index}`, { data: { text: String(index).padEnd(256 * 1024, 'x') } })
  }
  const retained = [...cache.keys()].reduce((bytes, key) => bytes + cache.get(key).data.text.length * 2, 0)
  assert.ok(retained <= 32 * 1024 * 1024, `retained ${retained} estimated text bytes`)
  cache.clear()
})

test('the SWR provider is memory-only, bounded, and removes legacy sensitive storage', () => {
  const browser = new Window({ url: 'https://tcp.1pc.tf/' })
  const restore = installTestDom(browser)
  try {
    browser.localStorage.setItem('rsctf-cache', 'old-private-snapshot')
    browser.localStorage.setItem('ad-api-token-7', 'ad_plaintext_secret')
    browser.localStorage.setItem('unrelated-preference', 'keep')
    clearLegacySensitiveBrowserStorage()
    assert.equal(browser.localStorage.getItem('rsctf-cache'), null)
    assert.equal(browser.localStorage.getItem('ad-api-token-7'), null)
    assert.equal(browser.localStorage.getItem('unrelated-preference'), 'keep')

    const cache = localCacheProvider()
    cache.clear()
    for (let index = 0; index < 700; index += 1) cache.set(`request-${index}`, { data: index })
    assert.equal(Array.from(cache.keys()).length, 512)
    assert.equal(cache.has('request-0'), false)
    assert.equal(cache.get('request-699')?.data, 699)
    assert.equal(browser.localStorage.getItem('rsctf-cache'), null)
  } finally {
    restore()
    browser.close()
  }
})

test('retiring an in-flight key rejects its stale metadata-only completion', () => {
  const cache = localCacheProvider()
  cache.clear()
  cache.set('private-key', { data: { owner: 'account-a' }, isValidating: true, _k: 'private-key' })
  assert.equal(retirePersistentCacheEntry(cache, 'private-key'), true)
  cache.set('private-key', { data: { owner: 'account-a' }, isValidating: false })
  assert.equal(cache.has('private-key'), false)

  cache.set('private-key', { data: { owner: 'account-b' }, _k: 'private-key' })
  assert.equal(cache.get('private-key')?.data.owner, 'account-b')
})
