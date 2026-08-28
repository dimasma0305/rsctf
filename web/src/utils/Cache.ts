import type { Cache } from 'swr'

/**
 * SWR is an in-process response cache, not durable application storage.
 *
 * Persisting its opaque state copied account, team, challenge, container and
 * administrator responses into browser storage without a response-cache
 * contract. Keep it memory-only so `private, no-store` responses can never
 * survive a reload, crash or account switch.
 */
const LEGACY_CACHE_KEY = 'rsctf-cache'
const LEGACY_IDB_DB_NAME = 'rsctf-cache'
export const MAX_SWR_CACHE_ENTRIES = 512
export const MAX_SWR_CACHE_BYTES = 2 * 1024 * 1024
const MAX_ENTRY_BYTES = 128 * 1024
const ENTRY_TTL_MS = 30 * 60 * 1000
const MAX_RETIRED_IN_FLIGHT_KEYS = 512

export const VIEWER_SCOPE_MARKER = 'rsctf-viewer-scope'

interface CacheMetadata {
  bytes: number
  expiresAt: number
}

type SwrCacheValue = Parameters<Cache<unknown>['set']>[1]

/** Estimate an entry without serialising the complete response. */
const boundedSize = (root: unknown, limit = MAX_ENTRY_BYTES): number => {
  const seen = new Set<object>()
  const pending: unknown[] = [root]
  let bytes = 0

  while (pending.length > 0 && bytes <= limit) {
    const value = pending.pop()
    switch (typeof value) {
      case 'string':
        bytes += value.length * 2
        break
      case 'number':
      case 'bigint':
        bytes += 8
        break
      case 'boolean':
        bytes += 4
        break
      case 'symbol':
      case 'function':
        bytes += 16
        break
      case 'object': {
        if (value === null || seen.has(value)) break
        seen.add(value)
        bytes += 32
        if (ArrayBuffer.isView(value)) {
          bytes += value.byteLength
          break
        }
        if (value instanceof ArrayBuffer) {
          bytes += value.byteLength
          break
        }
        if (Array.isArray(value)) {
          for (let index = value.length - 1; index >= 0 && bytes <= limit; index -= 1) pending.push(value[index])
          break
        }
        for (const [key, child] of Object.entries(value)) {
          bytes += key.length * 2
          pending.push(child)
          if (bytes > limit) break
        }
        break
      }
      default:
        break
    }
  }

  return bytes
}

class BoundedMemoryCache implements Cache<unknown> {
  private readonly map = new Map<string, SwrCacheValue>()
  private readonly metadata = new Map<string, CacheMetadata>()
  private readonly retiredInFlightKeys = new Set<string>()
  private totalBytes = 0

  get size() {
    this.pruneExpired()
    return this.map.size
  }

  get(key: string) {
    const metadata = this.metadata.get(key)
    if (metadata && metadata.expiresAt <= Date.now()) {
      this.remove(key)
      return undefined
    }
    return this.map.get(key)
  }

  has(key: string) {
    return this.get(key) !== undefined
  }

  set(key: string, value: SwrCacheValue) {
    if (this.retiredInFlightKeys.delete(key) && !Object.prototype.hasOwnProperty.call(value ?? {}, '_k')) {
      return this
    }

    const bytes = Math.min(MAX_ENTRY_BYTES + 1, boundedSize(key) + boundedSize(value))
    this.remove(key)
    if (bytes > MAX_ENTRY_BYTES) return this

    this.map.set(key, value)
    this.metadata.set(key, { bytes, expiresAt: Date.now() + ENTRY_TTL_MS })
    this.totalBytes += bytes
    this.evictToBudget()
    return this
  }

  delete(key: string) {
    this.retiredInFlightKeys.delete(key)
    return this.remove(key)
  }

  clear() {
    this.map.clear()
    this.metadata.clear()
    this.retiredInFlightKeys.clear()
    this.totalBytes = 0
  }

  retire(key: string) {
    const value = this.map.get(key) as { isValidating?: boolean } | undefined
    const removed = this.remove(key)
    if (value?.isValidating) {
      if (this.retiredInFlightKeys.size >= MAX_RETIRED_IN_FLIGHT_KEYS) {
        const oldest = this.retiredInFlightKeys.values().next().value
        if (oldest !== undefined) this.retiredInFlightKeys.delete(oldest)
      }
      this.retiredInFlightKeys.add(key)
    }
    return removed
  }

  keys() {
    this.pruneExpired()
    return this.map.keys()
  }

  values() {
    this.pruneExpired()
    return this.map.values()
  }

  entries() {
    this.pruneExpired()
    return this.map.entries()
  }

  [Symbol.iterator]() {
    return this.entries()
  }

  private remove(key: string) {
    const metadata = this.metadata.get(key)
    if (metadata) this.totalBytes -= metadata.bytes
    this.metadata.delete(key)
    return this.map.delete(key)
  }

  private pruneExpired(now = Date.now()) {
    for (const [key, metadata] of this.metadata) {
      if (metadata.expiresAt > now) continue
      this.remove(key)
    }
  }

  private evictToBudget() {
    this.pruneExpired()
    while (this.map.size > MAX_SWR_CACHE_ENTRIES || this.totalBytes > MAX_SWR_CACHE_BYTES) {
      const oldest = this.map.keys().next().value
      if (oldest === undefined) break
      this.remove(oldest)
    }
  }
}

const inMemoryCache = new BoundedMemoryCache()
let legacyPurgeStarted = false

const purgeLegacyPersistentCache = () => {
  if (legacyPurgeStarted || typeof window === 'undefined') return
  legacyPurgeStarted = true
  try {
    window.localStorage.removeItem(LEGACY_CACHE_KEY)
  } catch {
    // Storage can be disabled by browser policy. No new persistent write occurs.
  }
  try {
    if (typeof window.indexedDB !== 'undefined') window.indexedDB.deleteDatabase(LEGACY_IDB_DB_NAME)
  } catch {
    // Best-effort migration cleanup only; the cache no longer reads IndexedDB.
  }
}

/** Drop an exact SWR entry while fencing a late metadata-only in-flight write. */
export const retirePersistentCacheEntry = (cache: Cache, key: string) => {
  if (cache instanceof BoundedMemoryCache) return cache.retire(key)
  cache.delete(key)
  return true
}

/** No hydration exists; retained only as a compatibility hook for scope retirement. */
export const retirePersistentCacheScope = (_cache: Cache, _scope: string) => undefined

export const localCacheProvider = (): Cache<unknown> => {
  purgeLegacyPersistentCache()
  return inMemoryCache
}

export const clearLocalCache = () => {
  inMemoryCache.clear()
  purgeLegacyPersistentCache()
  if (typeof window !== 'undefined') window.location.reload()
}
