import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

test('one admin inventory hook samples every row in one paginated request', () => {
  const page = readFileSync('src/pages/admin/Instances.tsx', 'utf8')
  const filters = readFileSync('src/utils/AdminInstanceFilters.ts', 'utf8')
  const api = readFileSync('src/Api.ts', 'utf8')

  assert.equal((page.match(/useAdminInstancesPage\(/g) ?? []).length, 1)
  assert.equal((page.match(/useAdminGetInstanceStats\(/g) ?? []).length, 0)
  assert.match(filters, /count:\s*ADMIN_INSTANCE_PAGE_SIZE/)
  assert.match(filters, /skip:\s*\(state\.page - 1\) \* ADMIN_INSTANCE_PAGE_SIZE/)
  assert.match(filters, /includeRuntimeStats:\s*liveStats/)
  assert.match(page, /const total = instances\?\.total \?\? 0/)
  assert.match(api, /useAdminInstancesPage:[\s\S]*?doFetch \? \[`\/api\/admin\/instances`, query\] : null/)
  assert.match(api, /adminInstances: \(params: RequestParams = \{\}\)/)
})

test('team and challenge filters are server-authoritative and discover options beyond the page', () => {
  const page = readFileSync('src/pages/admin/Instances.tsx', 'utf8')
  const filters = readFileSync('src/utils/AdminInstanceFilters.ts', 'utf8')
  const api = readFileSync('src/Api.ts', 'utf8')

  assert.equal((page.match(/useAdminInstanceFilterOptions\(/g) ?? []).length, 2)
  assert.match(api, /path: `\/api\/admin\/instances\/filter-options`/)
  assert.match(filters, /teamId:\s*state\.team\?\.id/)
  assert.match(filters, /challengeId:\s*state\.challenge\?\.id/)
  assert.doesNotMatch(page, /filteredInstances|instances\?\.data\.filter/)
  assert.match(page, /filter=\{\(\{ options \}\) => options\}/)
})

test('authoritative filters expose accessible search scope, result status, and empty state', () => {
  const page = readFileSync('src/pages/admin/Instances.tsx', 'utf8')

  assert.match(page, /aria-label=\{t\('admin\.label\.instances\.team_filter'/)
  assert.match(page, /aria-label=\{t\('admin\.label\.instances\.challenge_filter'/)
  assert.equal((page.match(/aria-describedby="admin-instance-filter-help admin-instance-filter-status"/g) ?? []).length, 2)
  assert.match(page, /id="admin-instance-filter-status"[\s\S]*role="status"[\s\S]*aria-live="polite"/)
  assert.match(page, /no_filtered_results/)
  assert.match(page, /<Table\.Td colSpan=\{9\}>/)
})

test('unknown backend metrics are presented as unavailable instead of zero', () => {
  const page = readFileSync('src/pages/admin/Instances.tsx', 'utf8')

  assert.match(page, /memoryLimitBytes === 'number'/)
  assert.match(page, /limit_unavailable/)
  assert.match(page, /netRxBytes === 'number'/)
  assert.doesNotMatch(page, /HunamizeSize\(stats\.memoryLimitBytes \?\? 0\)/)
  assert.doesNotMatch(page, /HunamizeSize\(stats\.netRxBytes \?\? 0\)/)
})
