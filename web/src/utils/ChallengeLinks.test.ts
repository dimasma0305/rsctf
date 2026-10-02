import assert from 'node:assert/strict'
import test from 'node:test'
import { catalogChallengeHash, catalogChallengeIdFromHash, eventChallengeHash } from './ChallengeLinks'

test('catalog challenge hashes select bounded exact IDs, never ambiguous or malformed values', () => {
  for (const id of [1, 123, 2147483647]) {
    assert.equal(catalogChallengeIdFromHash(catalogChallengeHash('', id)), id)
  }
  for (const hash of [
    '',
    '#anchor',
    '#challenge=',
    '#challenge=0',
    '#challenge=-1',
    '#challenge=1.5',
    '#challenge=1e2',
    '#challenge=01',
    '#challenge=123junk',
    '#challenge=2147483648',
    '#challenge=9007199254740993',
    '#challenge=1&challenge=2',
    '#challenge=%FF',
    '#challenge=https://example.test',
  ]) {
    assert.equal(catalogChallengeIdFromHash(hash), null, hash)
  }
})

test('catalog card selection and close preserve unrelated fragment state', () => {
  const hash = catalogChallengeHash('#section=files&challenge=12&file=%2Fnotes%20one', 34)
  assert.equal(hash, '#section=files&challenge=34&file=%2Fnotes+one')
  assert.equal(catalogChallengeHash(hash, null), '#section=files&file=%2Fnotes+one')
  assert.equal(catalogChallengeHash('#challenge=34', null), '')
  assert.equal(catalogChallengeHash('#challenge=1&challenge=2', 34), '#challenge=34')
})

test('event card links preserve existing title-slug bookmarks and encode special characters', () => {
  assert.equal(eventChallengeHash(123, 'One # /?&'), '#123-One-%23-%2F%3F%26')
  assert.equal(
    new URL('/games/19/challenges' + eventChallengeHash(123, 'One # /?&'), 'https://tcp.1pc.tf').pathname,
    '/games/19/challenges'
  )
})
