import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import type { ScopedMutator } from 'swr'
import {
  invalidatePostPageCaches,
  isPostPageCacheKey,
  POST_FEED_JITTER_MS,
  POST_FEED_REFRESH_MS,
  postFeedRefreshDelay,
  postFeedSWRConfig,
} from './PostFeed'

test('homepage polling is jittered and pauses in hidden or offline tabs', () => {
  assert.equal(
    postFeedRefreshDelay(() => -1),
    POST_FEED_REFRESH_MS - POST_FEED_JITTER_MS
  )
  assert.equal(
    postFeedRefreshDelay(() => 0.5),
    POST_FEED_REFRESH_MS
  )
  assert.equal(
    postFeedRefreshDelay(() => 2),
    POST_FEED_REFRESH_MS + POST_FEED_JITTER_MS
  )
  assert.equal(postFeedSWRConfig.refreshWhenHidden, false)
  assert.equal(postFeedSWRConfig.refreshWhenOffline, false)
})

test('post page invalidation selects every visited pagination key only', () => {
  assert.equal(isPostPageCacheKey(['/api/posts/page', { count: 10, skip: 0 }]), true)
  assert.equal(isPostPageCacheKey(['/api/posts/page', { count: 10, skip: 10 }]), true)
  assert.equal(isPostPageCacheKey('/api/posts/page'), false)
  assert.equal(isPostPageCacheKey(['/api/posts/latest']), false)
})

test('post page invalidation uses the caller SWRConfig cache boundary', async () => {
  const calls: Parameters<ScopedMutator>[] = []
  const mutateCache = ((...args: Parameters<ScopedMutator>) => {
    calls.push(args)
    return Promise.resolve([])
  }) as ScopedMutator

  await invalidatePostPageCaches(mutateCache)

  assert.equal(calls.length, 1)
  assert.equal(calls[0][0], isPostPageCacheKey)
  assert.equal(calls[0][1], undefined)
  assert.deepEqual(calls[0][2], { populateCache: false, revalidate: true })
})

test('news page uses the explicit-total server page without client-side slicing', () => {
  const page = readFileSync('src/pages/posts/Index.tsx', 'utf8')
  const home = readFileSync('src/pages/Index.tsx', 'utf8')

  assert.match(page, /useInfoGetPostsPage/)
  assert.match(page, /postPage\?\.total/)
  assert.doesNotMatch(page, /posts\s*\?\.slice/)
  assert.match(home, /postFeedSWRConfig/)
})

test('every admin post mutation passes its SWRConfig-bound mutator to page invalidation', () => {
  const feed = readFileSync('src/utils/PostFeed.ts', 'utf8')
  const postIndex = readFileSync('src/pages/posts/Index.tsx', 'utf8')
  const postEdit = readFileSync('src/pages/posts/[postId]/Edit.tsx', 'utf8')
  const home = readFileSync('src/pages/Index.tsx', 'utf8')
  const mutationCalls = (source: string) => source.match(/api\.edit\.edit(?:Add|Delete|Update)Post\(/g)?.length ?? 0

  assert.doesNotMatch(feed, /import\s*\{\s*mutate\s*\}\s*from\s*['"]swr['"]/)
  for (const source of [postIndex, postEdit, home]) {
    assert.match(source, /useSWRConfig\(\)/)
  }
  assert.equal(mutationCalls(postIndex), 1)
  assert.equal(mutationCalls(postEdit), 3)
  assert.equal(mutationCalls(home), 1)
  assert.equal(postIndex.match(/invalidatePostPageCaches\(mutateCache\)/g)?.length, 1)
  assert.equal(postEdit.match(/await publishPostCaches\(\)/g)?.length, 2)
  assert.equal(postEdit.match(/invalidatePostPageCaches\(mutateCache\)/g)?.length, 2)
  assert.equal(home.match(/invalidatePostPageCaches\(mutateCache\)/g)?.length, 1)
})
