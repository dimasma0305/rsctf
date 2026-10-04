import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { installTestDom } from '../test/installDom'
import {
  clearLegacySensitiveBrowserStorage,
  localCacheProvider,
  retirePersistentCacheEntry,
} from './Cache'

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
