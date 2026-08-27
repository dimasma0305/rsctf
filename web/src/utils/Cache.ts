import LZString from 'lz-string'
import { gzip, ungzip } from 'pako'
import type { Cache } from 'swr'

// -----------------------------------------
// SWR Persistent Cache (Improved)
// -----------------------------------------

const CACHE_KEY = 'rsctf-cache'
const IDB_DB_NAME = 'rsctf-cache'
const IDB_STORE = 'swr'
const IDB_KEY = 'cache-map'
const MAX_RETIRED_IN_FLIGHT_KEYS = 512
const MAX_RETIRED_HYDRATION_SCOPES = 512

export const VIEWER_SCOPE_MARKER = 'rsctf-viewer-scope'

type BinaryLike = Uint8Array | ArrayBuffer

const cachedViewerScope = (value: unknown): string | null => {
  const originalKey = (value as { _k?: unknown } | null | undefined)?._k
  return Array.isArray(originalKey) &&
    originalKey.length === 3 &&
    originalKey[0] === VIEWER_SCOPE_MARKER &&
    typeof originalKey[1] === 'string'
    ? originalKey[1]
    : null
}

class PersistentCache implements Cache<any> {
  private map = new Map<any, any>()
  private retiredInFlightKeys = new Set<string>()
  private retiredHydrationScopes = new Set<string>()
  private dropViewerHydrationEntries = false
  private hydrationOpen = true

  get size() {
    return this.map.size
  }

  // Basic Map interface required by SWR
  get(key: any) {
    return this.map.get(key)
  }
  has(key: any) {
    return this.map.has(key)
  }
  set(key: any, value: any) {
    const scope = cachedViewerScope(value)
    if (scope && !this.dropViewerHydrationEntries) this.retiredHydrationScopes.delete(scope)
    if (this.retiredInFlightKeys.delete(key) && !Object.prototype.hasOwnProperty.call(value ?? {}, '_k')) {
      return this
    }
    this.map.set(key, value)
    schedulePersist()
    return this
  }
  delete(key: any) {
    this.retiredInFlightKeys.delete(key)
    const r = this.map.delete(key)
    schedulePersist()
    return r as any
  }
  clear() {
    this.map.clear()
    this.retiredInFlightKeys.clear()
    this.retiredHydrationScopes.clear()
    this.dropViewerHydrationEntries = false
    schedulePersist()
  }

  retire(key: string) {
    const value = this.map.get(key) as { isValidating?: boolean } | undefined
    const removed = this.map.delete(key)
    if (value?.isValidating) {
      if (this.retiredInFlightKeys.size >= MAX_RETIRED_IN_FLIGHT_KEYS) {
        const oldest = this.retiredInFlightKeys.values().next().value
        if (oldest !== undefined) this.retiredInFlightKeys.delete(oldest)
      }
      this.retiredInFlightKeys.add(key)
    }
    schedulePersist()
    return removed
  }
  retireScope(scope: string) {
    if (!this.hydrationOpen || this.dropViewerHydrationEntries) return
    if (this.retiredHydrationScopes.size >= MAX_RETIRED_HYDRATION_SCOPES) {
      this.retiredHydrationScopes.clear()
      this.dropViewerHydrationEntries = true
    } else {
      this.retiredHydrationScopes.add(scope)
    }
    // Rewrite the persistent snapshot even when the retired scope had not yet
    // reached memory. Its stale entries may still be waiting in the IDB read.
    schedulePersist()
  }
  finishHydration() {
    this.hydrationOpen = false
    this.retiredHydrationScopes.clear()
    this.dropViewerHydrationEntries = false
  }
  // Iteration
  keys() {
    return this.map.keys()
  }
  values() {
    return this.map.values()
  }
  entries() {
    return this.map.entries()
  }
  [Symbol.iterator]() {
    return this.map[Symbol.iterator]()
  }
  forEach(cb: (value: any, key: any, map: Map<any, any>) => void, thisArg?: any) {
    return this.map.forEach(cb as any, thisArg)
  }

  // Bulk hydrate (from_iter style)
  bulkAdd(entries: [any, any][]) {
    if (!entries.length) return
    let added = 0
    let filtered = false
    for (const [k, v] of entries) {
      const scope = cachedViewerScope(v)
      if (scope && (this.dropViewerHydrationEntries || this.retiredHydrationScopes.has(scope))) {
        filtered = true
        continue
      }
      if (this.retiredInFlightKeys.has(k)) continue
      if (!this.map.has(k)) {
        this.map.set(k, v)
        added++
      }
    }
    if (added || filtered) {
      dirty = true
      schedulePersist()
    }
    return added
  }

  snapshotEntries() {
    return Array.from(this.map.entries())
  }
}

/**
 * Delete through the persistent provider's retirement path when available.
 * Its bounded in-flight fence drops SWR's final metadata-only write after a
 * request was invalidated, while an explicit new hook owner (`_k`) can reuse
 * the same namespace safely.
 */
export const retirePersistentCacheEntry = (cache: Cache, key: string) => {
  if (cache instanceof PersistentCache) return cache.retire(key)
  cache.delete(key)
  return true
}

/** Keep an account namespace retired while its asynchronous IDB snapshot is loading. */
export const retirePersistentCacheScope = (cache: Cache, scope: string) => {
  if (cache instanceof PersistentCache) cache.retireScope(scope)
}

const inMemoryCache = new PersistentCache()

let idbSupported = typeof indexedDB !== 'undefined'
let dbPromise: Promise<IDBDatabase> | null = null
let hydrationStarted = false
let hydrated = false
let dirty = false
let persistTimer: number | null = null

const textEncoder = new TextEncoder()
const textDecoder = new TextDecoder()

const openDB = (): Promise<IDBDatabase> => {
  if (!idbSupported) return Promise.reject(new Error('IndexedDB not supported'))
  if (dbPromise) return dbPromise
  dbPromise = new Promise((resolve, reject) => {
    const req = indexedDB.open(IDB_DB_NAME, 1)
    req.onupgradeneeded = () => {
      const db = req.result
      if (!db.objectStoreNames.contains(IDB_STORE)) {
        db.createObjectStore(IDB_STORE)
      }
    }
    req.onsuccess = () => resolve(req.result)
    req.onerror = () => reject(req.error)
  })
  return dbPromise
}

const encodeMap = (cache: PersistentCache): Uint8Array => {
  const json = JSON.stringify(cache.snapshotEntries())
  return gzip(textEncoder.encode(json))
}

const decodeMap = (bin: BinaryLike): [any, any][] => {
  const u8 = bin instanceof Uint8Array ? bin : new Uint8Array(bin)
  const json = textDecoder.decode(ungzip(u8))
  return JSON.parse(json)
}

const fallbackHydrateLocalStorage = () => {
  try {
    const raw = localStorage.getItem(CACHE_KEY)
    if (!raw) return
    const decompressed = LZString.decompress(raw)
    if (!decompressed) return
    const entries: [any, any][] = JSON.parse(decompressed || '[]')
    inMemoryCache.bulkAdd(entries)
  } catch (e) {
    console.warn('[cache] localStorage hydrate failed', e)
  }
}

const fallbackPersistLocalStorage = () => {
  try {
    const serialized = JSON.stringify(inMemoryCache.snapshotEntries())
    const compressed = LZString.compress(serialized)
    localStorage.setItem(CACHE_KEY, compressed)
  } catch (e) {
    console.warn('[cache] fallback localStorage persist failed', e)
  }
}

const persistToIDB = async () => {
  if (!idbSupported || !dirty) return
  dirty = false
  try {
    const db = await openDB()
    const tx = db.transaction(IDB_STORE, 'readwrite')
    const store = tx.objectStore(IDB_STORE)
    const data = encodeMap(inMemoryCache)
    store.put(data, IDB_KEY)
    tx.onabort = () => console.warn('[cache] persist aborted', tx.error)
  } catch (e) {
    console.warn('[cache] persist failed, falling back to localStorage', e)
    fallbackPersistLocalStorage()
  }
}

const schedulePersist = () => {
  dirty = true
  if (persistTimer != null) return
  persistTimer = window.setTimeout(() => {
    persistTimer = null
    void persistToIDB()
  }, 3000)
}

const hydrateFromIDB = async () => {
  if (!idbSupported || hydrated) return
  hydrationStarted = true
  try {
    const db = await openDB()
    const tx = db.transaction(IDB_STORE, 'readonly')
    const store = tx.objectStore(IDB_STORE)
    const req = store.get(IDB_KEY)
    req.onsuccess = () => {
      try {
        const data = req.result as BinaryLike | undefined
        if (data) {
          const decoded = decodeMap(data)
          const added = inMemoryCache.bulkAdd(decoded)
          if (added) console.info('[cache] hydrated from IndexedDB, new entries:', added)
        }
      } catch (e) {
        console.warn('[cache] decode failed, attempting legacy migration', e)
        fallbackHydrateLocalStorage()
      } finally {
        hydrated = true
        inMemoryCache.finishHydration()
      }
    }
    req.onerror = () => {
      console.warn('[cache] IndexedDB read failed, using legacy localStorage', req.error)
      fallbackHydrateLocalStorage()
      hydrated = true
      inMemoryCache.finishHydration()
    }
  } catch (e) {
    console.warn('[cache] openDB failed, falling back to localStorage', e)
    idbSupported = false
    fallbackHydrateLocalStorage()
    hydrated = true
    inMemoryCache.finishHydration()
  }
}

const flushAndFallback = () => {
  if (idbSupported) void persistToIDB()
  else fallbackPersistLocalStorage()
}

const setupPersistenceSideEffects = () => {
  if (typeof window === 'undefined') return
  if (!hydrationStarted) void hydrateFromIDB()
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) flushAndFallback()
  })
  window.addEventListener(
    'beforeunload',
    () => {
      flushAndFallback()
    },
    { capture: true }
  )
}

export const localCacheProvider = (): Cache<any> => {
  setupPersistenceSideEffects()
  if (!hydrationStarted && !hydrated) {
    fallbackHydrateLocalStorage()
    hydrated = true
    inMemoryCache.finishHydration()
  }
  return inMemoryCache
}

export const clearLocalCache = () => {
  ;(async () => {
    try {
      if (idbSupported) {
        const db = await openDB()
        const tx = db.transaction(IDB_STORE, 'readwrite')
        tx.objectStore(IDB_STORE).delete(IDB_KEY)
      }
    } catch (e) {
      console.warn('[cache] clear idb failed', e)
    }
    try {
      localStorage.removeItem(CACHE_KEY)
    } catch {}
    inMemoryCache.clear()
    window.location.reload()
  })()
}
