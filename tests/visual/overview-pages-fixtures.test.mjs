import assert from 'node:assert/strict'
import test from 'node:test'
import { fixture } from './overview-pages-fixtures.mjs'

test('overview fixtures block writes and do not expose admin data to guests or players', () => {
  for (const path of ['/api/game', '/api/posts/latest', '/api/admin/dashboard', '/hub', '/api/unknown']) {
    for (const method of ['POST', 'PUT', 'PATCH', 'DELETE']) assert.equal(fixture(path, method).status, 405)
  }
  for (const role of ['Guest', 'User']) assert.equal(fixture('/api/admin/dashboard', 'GET', 'normal', role).status, 403)
  assert.equal(fixture('/hub/game').status, 404)
})

test('overview fixtures honor search, membership, and empty/loading/error states', () => {
  assert.equal(fixture('/api/game?search=Packet').body.total, 1)
  assert.equal(fixture('/api/game?membership=joined').body.total, 2)
  assert.equal(fixture('/api/game?membership=notJoined').body.total, 1)
  assert.equal(fixture('/api/game?skip=1&count=1').body.data.length, 1)
  assert.equal(fixture('/api/game?search=absent').body.total, 0)
  assert.equal(fixture('/api/game', 'GET', 'empty').body.total, 0)
  assert.equal(fixture('/api/game', 'GET', 'loading').hold, true)
  assert.equal(fixture('/api/game', 'GET', 'error').status, 503)
  assert.equal(fixture('/api/posts/latest').body.length, 2)
})
