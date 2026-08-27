import type { ScopedMutator, SWRConfiguration } from 'swr'

export const POST_PAGE_PATH = '/api/posts/page'
export const POST_FEED_REFRESH_MS = 5 * 60 * 1000
export const POST_FEED_JITTER_MS = 60 * 1000

export const postFeedRefreshDelay = (random: () => number = Math.random): number => {
  const sample = Math.min(1, Math.max(0, random()))
  return Math.round(POST_FEED_REFRESH_MS - POST_FEED_JITTER_MS + sample * 2 * POST_FEED_JITTER_MS)
}

export const postFeedSWRConfig: SWRConfiguration = {
  refreshInterval: () => postFeedRefreshDelay(),
  refreshWhenHidden: false,
  refreshWhenOffline: false,
}

export const isPostPageCacheKey = (key: unknown): boolean =>
  Array.isArray(key) && key.length > 0 && key[0] === POST_PAGE_PATH

/** Revalidate every page already visited in this browser after an admin write. */
export const invalidatePostPageCaches = (mutateCache: ScopedMutator) =>
  mutateCache(isPostPageCacheKey, undefined, {
    populateCache: false,
    revalidate: true,
  })
