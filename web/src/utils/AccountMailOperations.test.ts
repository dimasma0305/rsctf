import assert from 'node:assert/strict'
import test from 'node:test'
import {
  clearAccountMailOperation,
  readAccountMailOperation,
  retainAccountMailOperation,
} from './AccountMailOperations'

const storage = () => {
  const values = new Map<string, string>()
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  }
}

test('account mail identity is tab-local and stable only for one canonical scope', () => {
  const tab = storage()
  const first = retainAccountMailOperation(
    tab,
    'registration',
    'player@example.test\0player',
    null,
    () => '00000000-0000-4000-8000-000000000001',
    1_000
  )
  assert.equal(
    retainAccountMailOperation(
      tab,
      'registration',
      first.scope,
      null,
      () => '00000000-0000-4000-8000-000000000002',
      1_001
    ).operationId,
    first.operationId
  )
  assert.equal(readAccountMailOperation(tab, 'registration', first.scope, 1_002)?.operationId, first.operationId)
  assert.equal(readAccountMailOperation(tab, 'registration', 'other@example.test\0other', 1_003), null)
})

test('account mail clear is owner checked', () => {
  const tab = storage()
  const now = Date.now()
  const owner = retainAccountMailOperation(
    tab,
    'email-change',
    'user-id\0next@example.test',
    null,
    () => '00000000-0000-4000-8000-000000000003',
    now
  )
  clearAccountMailOperation(tab, { ...owner, operationId: '00000000-0000-4000-8000-000000000004' })
  assert.equal(readAccountMailOperation(tab, owner.purpose, owner.scope, now + 1)?.operationId, owner.operationId)
  clearAccountMailOperation(tab, owner)
  assert.equal(readAccountMailOperation(tab, owner.purpose, owner.scope, now + 2), null)
})
