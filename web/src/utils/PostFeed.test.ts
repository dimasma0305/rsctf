import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import {
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

test('news page uses the explicit-total server page without client-side slicing', () => {
  const page = readFileSync('src/pages/posts/Index.tsx', 'utf8')
  const home = readFileSync('src/pages/Index.tsx', 'utf8')

  assert.match(page, /useInfoGetPostsPage/)
  assert.match(page, /postPage\?\.total/)
  assert.doesNotMatch(page, /posts\s*\?\.slice/)
  assert.match(home, /postFeedSWRConfig/)
})
