import assert from 'node:assert/strict'
import test from 'node:test'
import { clearCreateIntent, readCreateIntent, writeCreateIntent } from './DurableCreateIntent'

const storage = () => {
  const values = new Map<string, string>()
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => void values.set(key, value),
    removeItem: (key: string) => void values.delete(key),
  }
}

test('create intent survives reload and is cleared only by its owner', () => {
  const store = storage()
  const now = Date.now()
  const intent = writeCreateIntent(store, 'create', { title: 'same intent' }, 'operation-a', now)
  assert.deepEqual(readCreateIntent(store, 'create', now + 1_000), intent)
  clearCreateIntent(store, 'create', 'operation-b')
  assert.deepEqual(readCreateIntent(store, 'create', now + 1_000), intent)
  clearCreateIntent(store, 'create', 'operation-a')
  assert.equal(readCreateIntent(store, 'create', now + 1_000), null)
})

test('expired and malformed intents cannot be replayed', () => {
  const store = storage()
  writeCreateIntent(store, 'expired', { title: 'old' }, 'operation-a', 1_000)
  assert.equal(readCreateIntent(store, 'expired', 3_602_000), null)
  store.setItem('broken', '{')
  assert.equal(readCreateIntent(store, 'broken', 2_000), null)
})
