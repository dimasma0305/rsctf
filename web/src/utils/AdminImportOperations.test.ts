import assert from 'node:assert/strict'
import test from 'node:test'
import {
  clearAdminImportOperation,
  readAdminImportOperation,
  retainAdminImportOperation,
} from './AdminImportOperations'

const storage = () => {
  const values = new Map<string, string>()
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  }
}

test('admin import retains one operation for one normalized request digest', () => {
  const tab = storage()
  const first = retainAdminImportOperation(
    tab,
    'a'.repeat(64),
    null,
    () => '00000000-0000-4000-8000-000000000001',
    1_000
  )
  assert.equal(
    retainAdminImportOperation(tab, 'a'.repeat(64), null, () => '00000000-0000-4000-8000-000000000002', 1_001)
      .operationId,
    first.operationId
  )
  assert.equal(
    retainAdminImportOperation(tab, 'b'.repeat(64), first, () => '00000000-0000-4000-8000-000000000002', 1_002)
      .operationId,
    '00000000-0000-4000-8000-000000000002'
  )
})

test('admin import recovery state is owner checked and expires', () => {
  const tab = storage()
  const now = Date.now()
  const owner = retainAdminImportOperation(tab, 'c'.repeat(64), null, () => '00000000-0000-4000-8000-000000000003', now)
  clearAdminImportOperation(tab, '00000000-0000-4000-8000-000000000004')
  assert.equal(readAdminImportOperation(tab, now + 1)?.operationId, owner.operationId)
  clearAdminImportOperation(tab, owner.operationId)
  assert.equal(readAdminImportOperation(tab, now + 2), null)

  retainAdminImportOperation(tab, 'd'.repeat(64), null, () => '00000000-0000-4000-8000-000000000005', now)
  assert.equal(readAdminImportOperation(tab, now + 60 * 60_000 + 1), null)
})
