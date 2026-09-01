import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const page = readFileSync('src/pages/admin/games/[id]/AdOps.tsx', 'utf8')
const api = readFileSync('src/Api.ts', 'utf8')

test('live A&D forensics aborts obsolete file, change, and history requests', () => {
  assert.ok((page.match(/new AbortController\(\)/g) ?? []).length >= 4)
  assert.ok((page.match(/controller\.abort\(\)/g) ?? []).length >= 4)
  assert.match(page, /editAdFile\([\s\S]*?\{ signal: controller\.signal \}\)/)
  assert.match(page, /editAdSnapshotChanges\(gameId, sid, \{ signal: controller\.signal \}\)/)
  assert.match(page, /editAdServiceSnapshots\(gameId, sid, \{ signal: controller\.signal \}\)/)
  assert.match(page, /generation !== requestGeneration\.current/)
  assert.match(page, /generation === changesGeneration\.current/)
})

test('generated API forwards request cancellation options for both live routes', () => {
  assert.match(api, /editAdSnapshotChanges:[\s\S]*?\.\.\.params,/)
  assert.match(api, /editAdFile:[\s\S]*?\.\.\.params,/)
})

test('large previews bypass syntax highlighting and overload has a retry control', () => {
  assert.match(page, /MAX_HIGHLIGHT_CHARACTERS = 64 \* 1024/)
  assert.match(page, /code\.length > MAX_HIGHLIGHT_CHARACTERS/)
  assert.match(page, /setRetryGeneration\(\(value\) => value \+ 1\)/)
  assert.match(page, /setChangesRetry\(\(value\) => value \+ 1\)/)
})
