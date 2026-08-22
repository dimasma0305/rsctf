import assert from 'node:assert/strict'
import test from 'node:test'
import {
  ADMIN_BUILD_IMAGES_CACHE_KEY,
  ADMIN_BUILD_STORAGE_CACHE_KEY,
  refreshAdminBuildImageViews,
} from './AdminBuildImages'

test('build image mutations refresh inventory and storage views', async () => {
  const refreshed: string[] = []
  await refreshAdminBuildImageViews(async (key) => {
    refreshed.push(key)
  })

  assert.deepEqual(refreshed.sort(), [ADMIN_BUILD_IMAGES_CACHE_KEY, ADMIN_BUILD_STORAGE_CACHE_KEY].sort())
})
