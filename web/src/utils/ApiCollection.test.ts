import assert from 'node:assert/strict'
import test from 'node:test'
import { apiCollectionPageCount, apiCollectionView, decodeApiCollection } from './ApiCollection'

test('API collections accept the released raw-array response', () => {
  const items = [{ id: 1 }, { id: 2 }]

  assert.deepEqual(decodeApiCollection(items), {
    status: 'ready',
    items,
    total: 2,
    paginated: false,
  })
})

test('API collections accept the paginated response used by newer servers', () => {
  const items = [{ id: 1 }]

  assert.deepEqual(decodeApiCollection({ data: items, total: 1, length: 1 }), {
    status: 'ready',
    items,
    total: 1,
    paginated: true,
  })
})

test('API collections keep loading separate from malformed responses', () => {
  assert.deepEqual(decodeApiCollection(undefined), { status: 'loading' })
  assert.deepEqual(decodeApiCollection(null), { status: 'invalid' })
  assert.deepEqual(decodeApiCollection({ data: {} }), { status: 'invalid' })
  assert.deepEqual(decodeApiCollection({ data: [], length: 0 }), { status: 'invalid' })
  assert.deepEqual(decodeApiCollection({ data: [{}], length: 0, total: 1 }), { status: 'invalid' })
  assert.deepEqual(decodeApiCollection({ data: [{}], length: 1, total: 0 }), { status: 'invalid' })
  assert.deepEqual(decodeApiCollection('not a collection'), { status: 'invalid' })
})

test('a revalidation failure keeps a decoded cached collection visible', () => {
  const ready = decodeApiCollection([{ id: 1 }])

  assert.equal(apiCollectionView(ready, undefined), 'ready')
  assert.equal(apiCollectionView(ready, new Error('temporary failure')), 'stale')
  assert.equal(apiCollectionView({ status: 'loading' }, new Error('initial failure')), 'failed')
  assert.equal(apiCollectionView({ status: 'invalid' }, undefined), 'failed')
})

test('pagination does not clamp a requested page while its response is loading', () => {
  assert.equal(apiCollectionPageCount({ status: 'loading' }, 20), undefined)
  assert.equal(apiCollectionPageCount(decodeApiCollection([{ id: 1 }]), 20), undefined)
  assert.equal(apiCollectionPageCount(decodeApiCollection({ data: [], length: 0, total: 41 }), 20), 3)
})
