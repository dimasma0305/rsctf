import assert from 'node:assert/strict'
import test from 'node:test'
import {
  clearEventVpnOverrideOperation,
  readEventVpnOverrideOperation,
  retainEventVpnOverrideOperation,
} from './EventVpnOverrideOperations'

const storage = () => {
  const values = new Map<string, string>()
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  }
}

test('VPN override intent survives reload only for the exact bounded payload', () => {
  const tab = storage()
  const intent = {
    kind: 'create' as const,
    reason: 'incident response',
    durationMinutes: 15,
    expectedPolicyRevision: 7,
  }
  const first = retainEventVpnOverrideOperation(tab, 4, intent, () => '00000000-0000-4000-8000-000000000001', 1_000)
  assert.deepEqual(readEventVpnOverrideOperation(tab, 4, 1_001), first)
  assert.equal(
    retainEventVpnOverrideOperation(tab, 4, intent, () => '00000000-0000-4000-8000-000000000002', 1_002).operationId,
    first.operationId
  )
  assert.equal(
    retainEventVpnOverrideOperation(
      tab,
      4,
      { ...intent, durationMinutes: 16 },
      () => '00000000-0000-4000-8000-000000000002',
      1_003
    ).operationId,
    '00000000-0000-4000-8000-000000000002'
  )
})

test('VPN override clear is owner checked and stale state expires', () => {
  const tab = storage()
  const now = Date.now()
  const owner = retainEventVpnOverrideOperation(
    tab,
    9,
    {
      kind: 'revoke',
      overrideId: '00000000-0000-4000-8000-000000000009',
      expectedPolicyRevision: 3,
    },
    () => '00000000-0000-4000-8000-000000000003',
    now
  )
  clearEventVpnOverrideOperation(tab, 9, '00000000-0000-4000-8000-000000000099')
  assert.equal(readEventVpnOverrideOperation(tab, 9, now + 1)?.operationId, owner.operationId)
  clearEventVpnOverrideOperation(tab, 9, owner.operationId)
  assert.equal(readEventVpnOverrideOperation(tab, 9, now + 2), null)

  retainEventVpnOverrideOperation(
    tab,
    9,
    { kind: 'create', reason: 'temporary recovery', durationMinutes: 5, expectedPolicyRevision: 4 },
    () => owner.operationId,
    10_000
  )
  assert.equal(readEventVpnOverrideOperation(tab, 9, 10_000 + 2 * 60 * 60_000 + 1), null)
})
