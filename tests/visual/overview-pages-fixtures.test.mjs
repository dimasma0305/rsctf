import assert from 'node:assert/strict'
import test from 'node:test'
import { runInNewContext } from 'node:vm'
import { fixture, topDocumentScript } from './overview-pages-fixtures.mjs'

test('overview browser setup touches storage only in the expected top-level document', () => {
  const origin = 'https://tcp.1pc.tf'
  const source = `localStorage.setItem('fixture', 'ready');`
  const top = {}; top.top = top
  for (const blockedOrigin of ['null', 'https://example.invalid']) {
    let storageReads = 0
    const context = { window: top, location: { origin: blockedOrigin }, get localStorage() { storageReads++; throw new Error('Storage denied') } }
    assert.throws(() => runInNewContext(source, context))
    assert.ok(storageReads > 0)
    storageReads = 0
    assert.doesNotThrow(() => runInNewContext(topDocumentScript(source, origin), context))
    assert.equal(storageReads, 0)
  }
  let writes = 0
  const storage = { setItem: () => writes++ }
  runInNewContext(topDocumentScript(source, origin), { window: { top }, location: { origin }, localStorage: storage })
  assert.equal(writes, 0, 'same-origin child frames must also be left alone')
  runInNewContext(topDocumentScript(source, origin), { window: top, location: { origin }, localStorage: storage })
  assert.equal(writes, 1)
})

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
