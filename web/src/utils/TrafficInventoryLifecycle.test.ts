import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const page = readFileSync(new URL('../pages/games/[id]/monitor/Traffic.tsx', import.meta.url), 'utf8')

test('traffic navigation cancels superseded inventories and generation-binds results', () => {
  assert.match(page, /new LatestListRequest<T>\(\)/)
  assert.match(page, /owner\.current\.cancel\(\)/)
  assert.match(page, /generation\.current \+= 1/)
  assert.match(page, /\{ signal \}/)
  assert.match(page, /currentListSnapshotRows\(scope, snapshot\)/)
  assert.doesNotMatch(page, /useGameGetChallengeTraffic/)
  assert.doesNotMatch(page, /useGameGetTeamTrafficAll/)
})
