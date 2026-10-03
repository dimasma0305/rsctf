import assert from 'node:assert/strict'
import test from 'node:test'
import { createArenaFixture } from './attack-arena-fixtures.mjs'

test('spectator fixtures cover dense Jeopardy, mixed and pure KotH rosters', () => {
  const dense = createArenaFixture()
  assert.equal(dense.scoreboard.items.length, 49)
  assert.equal(dense.ad.challenges.length, 0)
  assert.equal(dense.koth.hills.length, 0)
  assert.equal(dense.responses['/api/config'].customTheme, '#e33f3d')
  const mixed = createArenaFixture('mixed', 8)
  assert.equal(mixed.ad.challenges.length, 1)
  assert.equal(mixed.koth.hills.length, 1)
  const koth = createArenaFixture('koth', 8)
  assert.equal(koth.ad.challenges.length, 0)
  assert.equal(koth.koth.teams.length, 8)
})

test('spectator fixtures remain anonymous and cannot write or fall through to real APIs', () => {
  const { fixture } = createArenaFixture()
  assert.equal(fixture('/api/account/profile').status, 401)
  assert.equal(fixture('/api/game/901/ad/scoreboard').status, 200)
  assert.equal(fixture('/api/game/901/ad/scoreboard', 'POST').status, 405)
  assert.equal(fixture('/api/game/27/ad/scoreboard').status, 404)
  assert.equal(fixture('/hub/user/negotiate', 'POST').status, 405)
})
