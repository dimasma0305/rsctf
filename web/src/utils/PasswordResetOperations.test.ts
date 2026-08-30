import assert from 'node:assert/strict'
import test from 'node:test'
import {
  clearPasswordResetOperation,
  passwordResetRequestSignature,
  readPasswordResetOperation,
  retainPasswordResetOperation,
} from './PasswordResetOperations'

const storage = () => {
  const values = new Map<string, string>()
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  }
}

test('password reset storage scope is stable without password-derived data', async () => {
  const first = await passwordResetRequestSignature('token-a', 'player@example.test')
  assert.equal(first, await passwordResetRequestSignature('token-a', 'player@example.test'))
  assert.notEqual(first, await passwordResetRequestSignature('token-b', 'player@example.test'))
})

test('password reset reuses only the same request operation after reload', () => {
  const tab = storage()
  const first = retainPasswordResetOperation(
    tab,
    'a'.repeat(64),
    null,
    () => '00000000-0000-4000-8000-000000000001',
    1_000
  )
  assert.equal(
    retainPasswordResetOperation(tab, 'a'.repeat(64), null, () => crypto.randomUUID(), 1_001).operationId,
    first.operationId
  )
  assert.notEqual(
    retainPasswordResetOperation(tab, 'b'.repeat(64), first, () => crypto.randomUUID(), 1_002).operationId,
    first.operationId
  )
})

test('password reset state clears only for its terminal owner', () => {
  const tab = storage()
  const owner = retainPasswordResetOperation(
    tab,
    'c'.repeat(64),
    null,
    () => '00000000-0000-4000-8000-000000000003',
    Date.now()
  )
  clearPasswordResetOperation(tab, '00000000-0000-4000-8000-000000000004')
  assert.equal(readPasswordResetOperation(tab)?.operationId, owner.operationId)
  clearPasswordResetOperation(tab, owner.operationId)
  assert.equal(readPasswordResetOperation(tab), null)
})
