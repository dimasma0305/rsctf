import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

test('repository binding pages stay idle except for active work and use bounded server pages', () => {
  const source = readFileSync('src/pages/admin/repo-bindings.tsx', 'utf8')

  assert.match(source, /latest as ArrayResponseOfRepoBindingInfoModel[^]*currentActivity[^]*\? 3000\s*:\s*0/)
  assert.match(source, /SCHEDULER_WAKE_DELAY_MS = 15_250/)
  assert.match(source, /count: BINDING_PAGE_SIZE, skip:/)
  assert.match(source, /count: HISTORY_PAGE_SIZE/)
  assert.match(source, /<Pagination/)
  assert.doesNotMatch(source, /refreshInterval:\s*3000/)
})
