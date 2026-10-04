import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
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

test('animation measurement tolerates only retired interception IDs and retains other failures', () => {
  const source = readFileSync(new URL('./arena-animation.mjs', import.meta.url), 'utf8')
  assert.match(source, /const generation = documentGeneration/)
  assert.match(source, /includes\('Invalid InterceptionId'\) && \(generation !== documentGeneration \|\| closing\)/)
  assert.match(source, /else errors.push\(String\(error\)\)/)
  assert.match(source, /documentGeneration\+\+\s+await cdp.send\('Page.navigate'/)
  assert.match(source, /assert.deepEqual\(errors,\[\]\)/)
  assert.match(source, /finally \{\s+closing = true/)
  assert.match(source, /await browser.close\(\)/)
})
