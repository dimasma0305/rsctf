import assert from 'node:assert/strict'
import test from 'node:test'
import {
  clearAdminPasswordResetOperation,
  readAdminPasswordResetOperation,
  retainAdminPasswordResetOperation,
} from './AdminPasswordResetOperations'

const ADMIN = '00000000-0000-4000-8000-000000000001'
const USER = '00000000-0000-4000-8000-000000000002'
const OPERATION = '00000000-0000-4000-8000-000000000003'

const storage = () => {
  const values = new Map<string, string>()
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  }
}

test('admin password reset reuses its tab-owned operation after reload', () => {
  const tab = storage()
  const first = retainAdminPasswordResetOperation(tab, ADMIN, USER, null, () => OPERATION, 1_000)
  assert.equal(first, OPERATION)
  assert.equal(
    retainAdminPasswordResetOperation(tab, ADMIN, USER, null, () => crypto.randomUUID(), 1_001),
    OPERATION
  )
})

test('only the matching terminal operation clears retained reset recovery', () => {
  const tab = storage()
  const now = Date.now()
  retainAdminPasswordResetOperation(tab, ADMIN, USER, null, () => OPERATION, now)
  clearAdminPasswordResetOperation(tab, ADMIN, USER, crypto.randomUUID())
  assert.equal(readAdminPasswordResetOperation(tab, ADMIN, USER, now + 1), OPERATION)
  clearAdminPasswordResetOperation(tab, ADMIN, USER, OPERATION)
  assert.equal(readAdminPasswordResetOperation(tab, ADMIN, USER, now + 2), null)
})
