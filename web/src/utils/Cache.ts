import type { Cache } from 'swr'
import { estimateCacheBytes } from './CacheWeight'

const MAX_CACHE_ENTRIES = 512
const MAX_CACHE_BYTES = 32 * 1024 * 1024
const MAX_RETIRED_IN_FLIGHT_KEYS = 512
const LEGACY_CACHE_KEY = 'rsctf-cache'
const LEGACY_TOKEN_PREFIX = 'ad-api-token-'

export const VIEWER_SCOPE_MARKER = 'rsctf-viewer-scope'

/**
 * SWR responses are authorization-scoped runtime state, not durable browser
 * data. Keep a weighted LRU in-memory cache so secrets, private responses,
 * and a long admin search history never reach IndexedDB/localStorage.
 */
export class BoundedMemoryCache implements Cache<any> {
  private readonly map = new Map<any, any>()
  private readonly weights = new Map<any, number>()
  private readonly consumers = new Map<any, Set<symbol>>()
  private dataWeights = new WeakMap<object, number>()
  private readonly retiredInFlightKeys = new Set<string>()
  private bytes = 0
  private evicted = 0

  constructor(
    private readonly maxEntries = MAX_CACHE_ENTRIES,
    private readonly maxBytes = MAX_CACHE_BYTES
  ) {}

  get size() {
    return this.map.size
  }

  get estimatedBytes() {
    return this.bytes
  }
  get evictions() {
    return this.evicted
  }

  get(key: any) {
    const value = this.map.get(key)
    if (value !== undefined) {
      this.map.delete(key)
      this.map.set(key, value)
    }
    return value
  }

  /** SWR's immutable data model is weighed once, including metadata-only writes. */
  private dataBytes(data: any): number {
    if (data === null || data === undefined) return 0
    if (typeof data === 'string') return data.length * 2
    if (typeof data !== 'object') return 8
    const cached = this.dataWeights.get(data)
    if (cached !== undefined) return cached
    const bytes = estimateCacheBytes(data, this.maxBytes)
    this.dataWeights.set(data, bytes)
    return bytes
  }

  private remove(key: any) {
    this.bytes -= this.weights.get(key) ?? 0
    this.weights.delete(key)
    return this.map.delete(key)
  }

  private prune() {
    if (this.map.size <= this.maxEntries && this.bytes <= this.maxBytes) return
    for (const key of this.map.keys()) {
      // Mounted hooks own their working data. Even an oversized response must
      // render; release immediately reapplies the budget when its last owner leaves.
      if (this.consumers.has(key)) continue
      this.retire(key)
      this.evicted += 1
      if (this.map.size <= this.maxEntries && this.bytes <= this.maxBytes) break
    }
  }

  retain(key: string) {
    const token = Symbol()
    const leases = this.consumers.get(key) ?? new Set<symbol>()
    leases.add(token)
    this.consumers.set(key, leases)
    return () => {
      if (this.consumers.get(key) !== leases || !leases.delete(token)) return
      if (leases.size === 0) this.consumers.delete(key)
      this.prune()
    }
  }

  has(key: any) {
    return this.map.has(key)
  }

  set(key: any, value: any) {
    if (this.retiredInFlightKeys.delete(key) && !Object.prototype.hasOwnProperty.call(value ?? {}, '_k')) {
      return this
    }
    this.remove(key)
    this.map.set(key, value)
    const data = value && Object.prototype.hasOwnProperty.call(value, 'data') ? value.data : value
    const bytes = 192 + (typeof key === 'string' ? key.length * 2 : 64) + this.dataBytes(data)
    this.weights.set(key, bytes)
    this.bytes += bytes
    this.prune()
    return this
  }

  delete(key: any) {
    this.retiredInFlightKeys.delete(key)
    return this.remove(key)
  }

  clear() {
    this.map.clear()
    this.weights.clear()
    this.consumers.clear()
    this.dataWeights = new WeakMap()
    this.bytes = 0
    this.evicted = 0
    this.retiredInFlightKeys.clear()
  }

  retire(key: string) {
    const value = this.map.get(key) as { isValidating?: boolean } | undefined
    const removed = this.remove(key)
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
    // SWR iterates keys and reads each entry during filtered mutations. Reads
    // update recency, so a live Map iterator would revisit moved keys forever.
    return Array.from(this.map.keys()).values()
  }

  values() {
    return Array.from(this.map.values()).values()
  }

  entries() {
    return Array.from(this.map.entries()).values()
  }

  [Symbol.iterator]() {
    return this.entries()
  }

  forEach(callback: (value: any, key: any, map: BoundedMemoryCache) => void, thisArg?: any) {
    for (const [key, value] of this.entries()) callback.call(thisArg, value, key, this)
  }
}

const inMemoryCache = new BoundedMemoryCache()
let legacyStorageCleared = false

/** Remove plaintext tokens and the former unbounded persistent SWR snapshot. */
export const clearLegacySensitiveBrowserStorage = () => {
  try {
    const storage = typeof window !== 'undefined' ? window.localStorage : undefined
    if (!storage) return
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
