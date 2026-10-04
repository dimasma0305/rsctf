import type { Cache } from 'swr'

const MAX_CACHE_ENTRIES = 512
const MAX_RETIRED_IN_FLIGHT_KEYS = 512
const LEGACY_CACHE_KEY = 'rsctf-cache'
const LEGACY_TOKEN_PREFIX = 'ad-api-token-'

export const VIEWER_SCOPE_MARKER = 'rsctf-viewer-scope'

/**
 * SWR responses are authorization-scoped runtime state, not durable browser
 * data. Keep a small LRU-like in-memory cache so secrets, private responses,
 * and a long admin search history never reach IndexedDB/localStorage.
 */
class BoundedMemoryCache implements Cache<any> {
  private readonly map = new Map<any, any>()
  private readonly retiredInFlightKeys = new Set<string>()

  get size() {
    return this.map.size
  }

  get(key: any) {
    return this.map.get(key)
  }

  has(key: any) {
    return this.map.has(key)
  }

  set(key: any, value: any) {
    if (this.retiredInFlightKeys.delete(key) && !Object.prototype.hasOwnProperty.call(value ?? {}, '_k')) {
      return this
    }
    this.map.delete(key)
    this.map.set(key, value)
    while (this.map.size > MAX_CACHE_ENTRIES) {
      const oldest = this.map.keys().next().value
      if (oldest === undefined) break
      this.map.delete(oldest)
    }
    return this
  }

  delete(key: any) {
    this.retiredInFlightKeys.delete(key)
    return this.map.delete(key)
  }

  clear() {
    this.map.clear()
    this.retiredInFlightKeys.clear()
  }

  retire(key: string) {
    const value = this.map.get(key) as { isValidating?: boolean } | undefined
    const removed = this.map.delete(key)
    if (value?.isValidating) {
      while (this.retiredInFlightKeys.size >= MAX_RETIRED_IN_FLIGHT_KEYS) {
        const oldest = this.retiredInFlightKeys.values().next().value
        if (oldest === undefined) break
        this.retiredInFlightKeys.delete(oldest)
      }
      this.retiredInFlightKeys.add(key)
    }
    return removed
  }

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

  forEach(callback: (value: any, key: any, map: Map<any, any>) => void, thisArg?: any) {
    return this.map.forEach(callback, thisArg)
  }
}

const inMemoryCache = new BoundedMemoryCache()
let legacyStorageCleared = false

/** Remove plaintext tokens and the former unbounded persistent SWR snapshot. */
export const clearLegacySensitiveBrowserStorage = () => {
  const storage = typeof window !== 'undefined' ? window.localStorage : undefined
  if (!storage) return
  try {
    storage.removeItem(LEGACY_CACHE_KEY)
    const tokenKeys: string[] = []
    for (let index = 0; index < storage.length; index += 1) {
      const key = storage.key(index)
      if (key?.startsWith(LEGACY_TOKEN_PREFIX)) tokenKeys.push(key)
    }
    tokenKeys.forEach((key) => storage.removeItem(key))
  } catch {
    // Storage can be unavailable in hardened/private browser contexts. The
    // active provider remains memory-only regardless.
  }
}

export const retirePersistentCacheEntry = (cache: Cache, key: string) => {
  if (cache instanceof BoundedMemoryCache) return cache.retire(key)
  cache.delete(key)
  return true
}

// Persistence no longer exists. Keep the compatibility entry point because
// ViewerIdentity calls it while an old asynchronous owner is being retired.
export const retirePersistentCacheScope = (_cache: Cache, _scope: string) => undefined

export const localCacheProvider = (): Cache<any> => {
  if (!legacyStorageCleared) {
    legacyStorageCleared = true
    clearLegacySensitiveBrowserStorage()
  }
  return inMemoryCache
}

export const clearLocalCache = () => {
  clearLegacySensitiveBrowserStorage()
  inMemoryCache.clear()
  if (typeof window !== 'undefined') window.location.reload()
}
