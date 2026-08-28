import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const page = readFileSync('src/pages/games/[id]/monitor/Traffic.tsx', 'utf8')

test('traffic navigation cancels superseded inventories and generation-binds results', () => {
  assert.match(page, /new LatestListRequest<T>\(\)/)
  assert.match(page, /owner\.current\.cancel\(\)/)
  assert.match(page, /generation\.current \+= 1/)
  assert.match(page, /\{ signal \}/)
  assert.match(page, /currentListSnapshotRows\(scope, snapshot\)/)
  assert.doesNotMatch(page, /useGameGetChallengeTraffic/)
  assert.doesNotMatch(page, /useGameGetTeamTrafficAll/)
})

test('traffic team and file inventories expose bounded metadata-backed pagination', () => {
  assert.match(page, /const TRAFFIC_PAGE_SIZE = 50/)
  assert.match(page, /gameGetChallengeTrafficPage/)
  assert.match(page, /gameGetTeamTrafficPage/)
  assert.match(page, /skip: \(teamPage - 1\) \* TRAFFIC_PAGE_SIZE/)
  assert.match(page, /skip: \(filePage - 1\) \* TRAFFIC_PAGE_SIZE/)
  assert.match(page, /const teamTotal = teamQuery\.page\?\.total/)
  assert.match(page, /const fileTotal = fileQuery\.page\?\.total/)
  assert.match(page, /if \(teamTotal === undefined\) return/)
  assert.match(page, /if \(fileTotal === undefined\) return/)
  assert.match(page, /<InventoryPager[\s\S]*Captured team pages/)
  assert.match(page, /<InventoryPager[\s\S]*Capture file pages/)
})
