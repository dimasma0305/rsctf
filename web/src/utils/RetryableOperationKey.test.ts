import assert from 'node:assert/strict'
import test from 'node:test'
import { RetryableOperationKey } from './RetryableOperationKey'

test('failed durable operations retain their key until an acknowledged completion', () => {
  const ids = ['operation-1', 'operation-2']
  const key = new RetryableOperationKey(() => ids.shift()!)

  const first = key.claim()
  assert.equal(first, 'operation-1')
  assert.equal(key.claim(), first)

  key.complete('another-operation')
  assert.equal(key.claim(), first)

  key.complete(first)
  assert.equal(key.claim(), 'operation-2')
})
