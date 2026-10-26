import { useLayoutEffect } from 'react'
import { unstable_serialize, useSWRConfig, type Middleware } from 'swr'
import { BoundedMemoryCache } from './Cache'

/** Retain only the mounted hook's serialized, already viewer-scoped key. */
export const boundedCacheMiddleware: Middleware = (useSWRNext) =>
  function useBoundedCache(key, fetcher, config) {
    const serialized = unstable_serialize(key)
    const { cache } = useSWRConfig()
    useLayoutEffect(() => {
      if (serialized && cache instanceof BoundedMemoryCache) return cache.retain(serialized)
    }, [cache, serialized])
    return useSWRNext(key, fetcher, config)
  }
