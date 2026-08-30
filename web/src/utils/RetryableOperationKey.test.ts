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

test('a component remount restores its pending operation until completion', () => {
  const values = new Map<string, string>()
  const storage = {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  }
  const firstId = '018f0000-0000-4000-8000-000000000001'
  const nextId = '018f0000-0000-4000-8000-000000000002'
  const firstMount = new RetryableOperationKey(() => firstId, 'game-13', storage)

  assert.equal(firstMount.claim(), firstId)
  assert.equal(new RetryableOperationKey(() => nextId, 'game-13', storage).claim(), firstId)

  const remount = new RetryableOperationKey(() => nextId, 'game-13', storage)
  remount.complete(firstId)
  assert.equal(new RetryableOperationKey(() => nextId, 'game-13', storage).claim(), nextId)
})

test('malformed session state cannot poison a later operation', () => {
  const values = new Map([['game-13', 'not-a-uuid']])
  const storage = {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  }
  const expected = '018f0000-0000-4000-8000-000000000003'

  assert.equal(new RetryableOperationKey(() => expected, 'game-13', storage).claim(), expected)
})
