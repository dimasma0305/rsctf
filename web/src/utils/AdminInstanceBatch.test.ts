import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

test('one admin inventory hook samples every row in one paginated request', () => {
  const page = readFileSync('src/pages/admin/Instances.tsx', 'utf8')
  const api = readFileSync('src/Api.ts', 'utf8')

  assert.equal((page.match(/useAdminInstancesPage\(/g) ?? []).length, 1)
  assert.equal((page.match(/useAdminGetInstanceStats\(/g) ?? []).length, 0)
  assert.match(page, /count:\s*ITEM_COUNT_PER_PAGE/)
  assert.match(page, /skip:\s*\(page - 1\) \* ITEM_COUNT_PER_PAGE/)
  assert.match(page, /includeRuntimeStats:\s*liveStats/)
  assert.match(page, /const total = instances\?\.total \?\? 0/)
  assert.match(api, /useAdminInstancesPage:[\s\S]*?doFetch \? \[`\/api\/admin\/instances`, query\] : null/)
  assert.match(api, /adminInstances: \(params: RequestParams = \{\}\)/)
})

test('unknown backend metrics are presented as unavailable instead of zero', () => {
  const page = readFileSync('src/pages/admin/Instances.tsx', 'utf8')

  assert.match(page, /memoryLimitBytes === 'number'/)
  assert.match(page, /limit_unavailable/)
  assert.match(page, /netRxBytes === 'number'/)
  assert.doesNotMatch(page, /HunamizeSize\(stats\.memoryLimitBytes \?\? 0\)/)
  assert.doesNotMatch(page, /HunamizeSize\(stats\.netRxBytes \?\? 0\)/)
})
