import assert from 'node:assert/strict'
import test from 'node:test'
import { readChallengeUpdateIntent, writeChallengeUpdateIntent } from './ChallengeUpdateIntent'

test('challenge update intent retains exact payload and observed revision', () => {
  const values = new Map<string, string>()
  const storage = {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => void values.set(key, value),
    removeItem: (key: string) => void values.delete(key),
  }
  const intent = writeChallengeUpdateIntent(storage, 'update', 7, { title: 'new' }, 'operation-a', 1_000)
  assert.deepEqual(readChallengeUpdateIntent(storage, 'update', 2_000), intent)
  assert.equal(intent.expectedRevision, 7)
})
