import assert from 'node:assert/strict'
import test from 'node:test'
import { fixture } from './refined-pages-fixtures.mjs'

test('refined-page fixtures block every write, including posts and unknown APIs', () => {
  for (const path of ['/api/posts/page', '/api/edit/games/19', '/api/unknown', '/hub']) {
    for (const method of ['POST', 'PUT', 'PATCH', 'DELETE']) assert.equal(fixture(path, method).status, 405)
  }
})

test('refined-page fixtures provide posts and preserve readiness evidence', () => {
  assert.equal(fixture('/api/posts/page').body.total, 1)
  assert.equal(fixture('/api/edit/games/19').body.hidden, true)
  assert.ok(fixture('/api/edit/games/19/challenges').body.some(challenge => challenge.buildStatus === 'Failed'))
})
