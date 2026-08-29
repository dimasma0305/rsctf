import assert from 'node:assert/strict'
import test from 'node:test'
import { decodeApiCollection } from './ApiCollection'

test('API collections accept the released raw-array response', () => {
  const items = [{ id: 1 }, { id: 2 }]

  assert.deepEqual(decodeApiCollection(items), { status: 'ready', items })
})

test('API collections accept the paginated response used by newer servers', () => {
  const items = [{ id: 1 }]

  assert.deepEqual(decodeApiCollection({ data: items, total: 1, length: 1 }), {
    status: 'ready',
    items,
  })
})

test('API collections keep loading separate from malformed responses', () => {
  assert.deepEqual(decodeApiCollection(undefined), { status: 'loading' })
  assert.deepEqual(decodeApiCollection(null), { status: 'invalid' })
  assert.deepEqual(decodeApiCollection({ data: {} }), { status: 'invalid' })
  assert.deepEqual(decodeApiCollection('not a collection'), { status: 'invalid' })
})
