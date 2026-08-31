import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'

const source = readFileSync('src/pages/games/[id]/monitor/Traffic.tsx', 'utf8')

test('traffic archive UI owns the actual fetch body and aborts it on navigation', () => {
  assert.match(source, /runDownloadSingleFlight\(scopeKey/)
  assert.match(source, /new AbortController\(\)/)
  assert.match(source, /await response\.blob\(\)/)
  assert.match(source, /downloadAllRequest\.current\?\.abort\(\)/)
  assert.doesNotMatch(source, /window\.open\(/)
  assert.doesNotMatch(source, /downloadAllRelease/)
  assert.doesNotMatch(source, /}, 5000\)/)
})

test('traffic archive errors and object URLs have explicit cleanup', () => {
  assert.match(source, /if \(!response\.ok\) throw await trafficArchiveFailure\(response\)/)
  assert.match(source, /window\.URL\.revokeObjectURL\(objectUrl\)/)
  assert.match(source, /if \(downloadAllObjectUrl\.current\) window\.URL\.revokeObjectURL/)
  assert.match(source, /showErrorMsg\(error, t\)/)
})
