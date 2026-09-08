import assert from 'node:assert/strict'
import test from 'node:test'
import { fixture } from './event-readiness-fixtures.mjs'

test('readiness fixtures isolate writes and model independent read failures', () => {
  for (const method of ['POST', 'PUT', 'PATCH', 'DELETE']) assert.equal(fixture('/api/edit/games/19', method).status, 405)
  for (const path of ['/api/edit/games/19', '/api/edit/games/19/challenges']) {
    assert.equal(fixture(path, 'GET', 'loading').hold, true)
    assert.equal(fixture(path, 'GET', 'failed').status, 503)
    assert.equal(fixture(path, 'GET', 'normal', 'User').status, 403)
    assert.ok(fixture(path, 'GET', 'normal', 'Manager').body)
  }
  assert.deepEqual(fixture('/api/edit/games/19/challenges', 'GET', 'empty').body, [])
})
