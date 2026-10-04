import assert from 'node:assert/strict'
import test from 'node:test'
import { createCompetitionFixture } from './competition-fixtures.mjs'

test('competition fixtures block mutations and unknown API reads', () => {
  const { fixture } = createCompetitionFixture()
  for (const path of ['/api/game/901/challenges/9001/submit', '/api/game/901', '/api/admin/config', '/hub/unknown']) {
    for (const method of ['POST', 'PUT', 'PATCH', 'DELETE']) assert.equal(fixture(path, method).status, 405)
  }
  assert.equal(fixture('/api/admin/config').status, 404)
  assert.equal(fixture('/api/game/901/challenges/999999').status, 404)
  assert.match(fixture('/hub/user/negotiate?game=901', 'POST').body.error, /read-only fixture/)
  assert.equal(fixture('/hub/user/negotiate', 'DELETE').status, 405)
})

test('competition fixtures distinguish rejected attempts and do not share mutable event state', () => {
  const first = createCompetitionFixture()
  assert.equal(first.challenges.length, 100)
  assert.equal(first.rank.solvedChallenges.filter(item => item.type !== 'Unaccepted').length, 12)
  assert.equal(first.rank.solvedChallenges.find(item => item.id === 9001).type, 'Unaccepted')
  first.game.practiceMode = true
  assert.equal(createCompetitionFixture().game.practiceMode, false)
  assert.equal(first.fixture('/api/game/901').body.practiceMode, true)
})
