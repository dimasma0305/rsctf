import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { gzip, ungzip } from 'pako'
import { act, createElement, type FC } from 'react'
import type { Key } from 'swr'
import { installTestDom } from '../test/installDom'

type ControlledRequest<T> = {
  result: T
  error: DOMException | null
  onsuccess: (() => void) | null
  onerror: (() => void) | null
  onupgradeneeded?: (() => void) | null
}

const request = <T>(result: T): ControlledRequest<T> => ({
  result,
  error: null,
  onsuccess: null,
  onerror: null,
})

const controlledIndexedDB = () => {
  let stored: Uint8Array | ArrayBuffer | undefined
  let pendingRead: ControlledRequest<Uint8Array | ArrayBuffer | undefined> | undefined
  let resolveReadReady!: () => void
  let writeCount = 0
  const writeWaiters: { target: number; resolve: () => void }[] = []
  const readReady = new Promise<void>((resolve) => {
    resolveReadReady = resolve
  })
  const objectStore = {
    get: (_key: IDBValidKey) => {
      pendingRead = request(stored)
      resolveReadReady()
      return pendingRead as unknown as IDBRequest
    },
    put: (value: Uint8Array | ArrayBuffer, _key: IDBValidKey) => {
      stored = value
      writeCount += 1
      for (const waiter of writeWaiters.splice(0)) {
        if (writeCount >= waiter.target) waiter.resolve()
        else writeWaiters.push(waiter)
      }
      return request(undefined) as unknown as IDBRequest
    },
    delete: (_key: IDBValidKey) => {
      stored = undefined
      return request(undefined) as unknown as IDBRequest
    },
  }
  const database = {
    objectStoreNames: { contains: () => true },
    transaction: () => ({
      error: null,
      onabort: null,
      objectStore: () => objectStore,
    }),
  }
  const factory = {
    open: () => {
      const openRequest = request(database)
      openRequest.onupgradeneeded = null
      queueMicrotask(() => openRequest.onsuccess?.())
      return openRequest as unknown as IDBOpenDBRequest
    },
  } as IDBFactory

  return {
    factory,
    readReady,
    seed(value: Uint8Array) {
      stored = value
    },
    releaseRead() {
      assert.ok(pendingRead, 'the cache must have an IndexedDB hydration read in flight')
      pendingRead.onsuccess?.()
    },
    nextWrite() {
      const target = writeCount + 1
      return new Promise<void>((resolve) => writeWaiters.push({ target, resolve }))
    },
    stored() {
      return stored
    },
  }
}

const waitUntil = async (condition: () => boolean) => {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    if (condition()) return
    await new Promise((resolve) => setTimeout(resolve, 0))
  }
  assert.fail('condition did not become true')
}

test('a pre-hydration account retirement cannot restore or persist its viewer namespace', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/17' })
  const restoreDom = installTestDom(browser)
  const previousIndexedDB = Object.getOwnPropertyDescriptor(globalThis, 'indexedDB')
  const previousLocalStorage = Object.getOwnPropertyDescriptor(globalThis, 'localStorage')
  const idb = controlledIndexedDB()
  Object.defineProperties(globalThis, {
    indexedDB: { configurable: true, value: idb.factory },
    localStorage: { configurable: true, value: browser.localStorage },
  })
  const { localCacheProvider, VIEWER_SCOPE_MARKER } = await import('./Cache')
  const { viewerIdentityMiddleware, ViewerIdentityProvider, viewerScopedKey } = await import('./ViewerIdentity')
  const { default: useSWR, SWRConfig, unstable_serialize, useSWRConfig } = await import('swr')
  const { MemoryRouter } = await import('react-router')
  const { createRoot } = await import('react-dom/client')
  const currentRequest = '/api/game/17/details'
  const stalePersistedRequest = '/api/game/18/details'
  const accountAScope = 'user:a:User'
  const accountBScope = 'user:b:User'
  const accountACurrentKey = unstable_serialize(viewerScopedKey(currentRequest, accountAScope))
  const accountAStaleKey = unstable_serialize(viewerScopedKey(stalePersistedRequest, accountAScope))
  const accountBCurrentKey = unstable_serialize(viewerScopedKey(currentRequest, accountBScope))
  const accountAStaleValue = {
    _k: viewerScopedKey(stalePersistedRequest, accountAScope),
    data: { label: 'persisted private account A data' },
  }
  idb.seed(gzip(new TextEncoder().encode(JSON.stringify([[accountAStaleKey, accountAStaleValue]]))))
  const cache = localCacheProvider()
  cache.set('/api/account/profile', { data: { userId: 'a', userName: 'account A', role: 'User' } } as never)
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  let mutateCache: ReturnType<typeof useSWRConfig>['mutate'] | undefined

  const Controls: FC = () => {
    mutateCache = useSWRConfig().mutate
    return null
  }
  const Probe: FC = () => {
    const { data } = useSWR<{ label: string }>(currentRequest)
    return createElement('output', null, data?.label ?? 'loading')
  }
  const App: FC = () =>
    createElement(
      SWRConfig,
      {
        value: {
          provider: () => cache,
          fetcher: (_key: Key) => new Promise<never>(() => undefined),
          dedupingInterval: 0,
          shouldRetryOnError: false,
          use: [viewerIdentityMiddleware],
        },
      },
      createElement(
        MemoryRouter,
        null,
        createElement(Controls),
        createElement(ViewerIdentityProvider, null, createElement(Probe))
      )
    )
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => {
      root.render(createElement(App))
      await idb.readReady
      await Promise.resolve()
    })
    assert.equal(cache.has(accountACurrentKey), true)
    assert.equal(cache.has(accountAStaleKey), false, 'the delayed persisted key must not be in memory yet')

    await act(async () => {
      await mutateCache?.(
        '/api/account/profile',
        { userId: 'b', userName: 'account B', role: 'User' },
        { revalidate: false }
      )
      await Promise.resolve()
    })
    await waitUntil(() => !cache.has(accountACurrentKey) && cache.has(accountBCurrentKey))

    await act(async () => {
      idb.releaseRead()
      await Promise.resolve()
    })
    assert.equal(cache.has(accountAStaleKey), false, 'hydration cannot restore another retired account key')

    const persisted = idb.nextWrite()
    browser.dispatchEvent(new browser.Event('beforeunload'))
    await persisted
    const stored = idb.stored()
    assert.ok(stored, 'retirement must rewrite the persistent snapshot')
    const entries = JSON.parse(new TextDecoder().decode(ungzip(stored))) as [string, { _k?: unknown }][]
    assert.equal(
      entries.some(([, value]) => {
        const originalKey = value?._k
        return Array.isArray(originalKey) && originalKey[0] === VIEWER_SCOPE_MARKER && originalKey[1] === accountAScope
      }),
      false,
      'the rewritten snapshot must not persist any retired account namespace'
    )
  } finally {
    await act(async () => root.unmount())
    for (const key of cache.keys()) cache.delete(key)
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
    if (previousIndexedDB) Object.defineProperty(globalThis, 'indexedDB', previousIndexedDB)
    else delete (globalThis as typeof globalThis & { indexedDB?: IDBFactory }).indexedDB
    if (previousLocalStorage) Object.defineProperty(globalThis, 'localStorage', previousLocalStorage)
    else delete (globalThis as typeof globalThis & { localStorage?: Storage }).localStorage
  }
})
