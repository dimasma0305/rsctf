import assert from 'node:assert/strict'
import test from 'node:test'
import { createChallengeLinksFixture } from './challenge-links-fixtures.mjs'

test('card link fixtures support exact off-page lookups and reject writes', () => {
  const { fixture } = createChallengeLinksFixture()
  assert.equal(fixture('/api/game/challenges?count=24').body.data.length, 24)
  assert.deepEqual(fixture('/api/game/challenges?challengeId=9099&count=1').body.data.map(item => item.id), [9099])
  assert.deepEqual(fixture('/api/game/challenges?challengeId=999999&count=1').body, { data: [], total: 0 })
  for (const method of ['POST', 'PUT', 'PATCH', 'DELETE']) {
    assert.equal(fixture('/api/game/challenges?challengeId=9099', method).status, 405)
    assert.equal(fixture('/api/game/901/challenges/9099', method).status, 405)
  }
})
