import assert from 'node:assert/strict'
import test from 'node:test'
import {
  claimPlayerCredentialOperation,
  clearPlayerCredentialOperation,
  ownsPlayerCredentialResult,
  readPlayerCredentialOperation,
} from './PlayerCredentialOperations'

const memoryStorage = () => {
  const values = new Map<string, string>()
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  }
}

test('reload and ambiguous-response retry retain one operation identity', () => {
  const storage = memoryStorage()
  const first = claimPlayerCredentialOperation(storage, 'token', 4, 1_000, () => 'first')
  assert.deepEqual(
    claimPlayerCredentialOperation(storage, 'token', 4, 2_000, () => 'second'),
    first
  )
  assert.deepEqual(
    claimPlayerCredentialOperation(storage, 'token', 5, 3_000, () => 'third'),
    first
  )
  assert.equal(ownsPlayerCredentialResult(storage, 'token', first, { operationId: 'first', revision: 5 }), true)
})

test('expired and superseded records are replaced without accepting stale responses', () => {
  const storage = memoryStorage()
  const first = claimPlayerCredentialOperation(storage, 'ssh', 2, 0, () => 'first')
  const expired = claimPlayerCredentialOperation(storage, 'ssh', 2, 15 * 60_000, () => 'second')
  assert.equal(expired.operationId, 'second')
  const superseded = claimPlayerCredentialOperation(storage, 'ssh', 7, 15 * 60_000 + 1, () => 'third')
  assert.equal(superseded.operationId, 'third')
  assert.equal(ownsPlayerCredentialResult(storage, 'ssh', first, { operationId: 'first', revision: 3 }), false)
  assert.equal(ownsPlayerCredentialResult(storage, 'ssh', superseded, { operationId: 'first', revision: 3 }), false)
  clearPlayerCredentialOperation(storage, 'ssh', 'wrong')
  assert.equal(readPlayerCredentialOperation(storage, 'ssh')?.operationId, 'third')
  clearPlayerCredentialOperation(storage, 'ssh', 'third')
  assert.equal(readPlayerCredentialOperation(storage, 'ssh'), null)
})
