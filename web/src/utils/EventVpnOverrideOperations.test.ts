import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import {
  clearEventVpnOverrideCreateOperation,
  clearEventVpnOverrideRevokeOperation,
  readEventVpnOverrideOperations,
  retainEventVpnOverrideCreateOperation,
  retainEventVpnOverrideRevokeOperation,
} from './EventVpnOverrideOperations'

const STORAGE_KEY = 'rsctf:event-vpn-override-operations'
const DAY_MS = 24 * 60 * 60_000

const uuid = (value: number): string => `00000000-0000-4000-8000-${value.toString().padStart(12, '0')}`
const signature = (reason: string, durationMinutes: number): string => `${reason}\0${durationMinutes}`

const storage = () => {
  const values = new Map<string, string>()
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  }
}

test('create recovery reuses the tab-owned identity and original policy revision', () => {
  const tab = storage()
  const first = retainEventVpnOverrideCreateOperation(
    tab,
    {
      gameId: 7,
      signature: signature('incident response', 15),
      reason: 'incident response',
      durationMinutes: 15,
      expectedPolicyRevision: 11,
    },
    null,
    () => uuid(1),
    1_000
  )

  const recovered = retainEventVpnOverrideCreateOperation(
    tab,
    {
      gameId: 7,
      signature: signature('incident response', 15),
      reason: 'incident response',
      durationMinutes: 15,
      expectedPolicyRevision: 99,
    },
    null,
    () => uuid(2),
    1_001
  )
  assert.equal(recovered.operationId, first.operationId)
  assert.equal(recovered.expectedPolicyRevision, 11)

  const changedIntent = retainEventVpnOverrideCreateOperation(
    tab,
    {
      gameId: 7,
      signature: signature('longer incident response', 30),
      reason: 'longer incident response',
      durationMinutes: 30,
      expectedPolicyRevision: 12,
    },
    recovered,
    () => uuid(2),
    1_002
  )
  assert.equal(changedIntent.operationId, uuid(2))
  assert.equal(changedIntent.expectedPolicyRevision, 12)
  assert.equal(readEventVpnOverrideOperations(tab, 7, 1_003).create?.operationId, uuid(2))
})

test('revoke recovery is scoped to the event and override and clears only its owner', () => {
  const tab = storage()
  const first = retainEventVpnOverrideRevokeOperation(
    tab,
    { gameId: 7, overrideId: uuid(10), expectedPolicyRevision: 21 },
    null,
    () => uuid(11),
    2_000
  )
  const second = retainEventVpnOverrideRevokeOperation(
    tab,
    { gameId: 7, overrideId: uuid(12), expectedPolicyRevision: 22 },
    null,
    () => uuid(13),
    2_001
  )
  const recovered = retainEventVpnOverrideRevokeOperation(
    tab,
    { gameId: 7, overrideId: uuid(10), expectedPolicyRevision: 99 },
    null,
    () => uuid(14),
    2_002
  )
  assert.equal(recovered.operationId, first.operationId)
  assert.equal(recovered.expectedPolicyRevision, 21)

  clearEventVpnOverrideRevokeOperation(tab, 7, uuid(10), uuid(99), 2_003)
  assert.equal(readEventVpnOverrideOperations(tab, 7, 2_004).revokes.length, 2)
  clearEventVpnOverrideRevokeOperation(tab, 7, uuid(10), first.operationId, 2_005)
  assert.deepEqual(
    readEventVpnOverrideOperations(tab, 7, 2_006).revokes.map((operation) => operation.operationId),
    [second.operationId]
  )
})

test('recovery state expires, rejects malformed state, and stays storage bounded', () => {
  const tab = storage()
  const unicodeReason = '🔐'.repeat(512)
  retainEventVpnOverrideCreateOperation(
    tab,
    {
      gameId: 7,
      signature: signature(unicodeReason, 15),
      reason: unicodeReason,
      durationMinutes: 15,
      expectedPolicyRevision: 1,
    },
    null,
    () => uuid(1),
    3_000
  )
  assert.equal(Array.from(readEventVpnOverrideOperations(tab, 7, 3_001).create?.reason ?? '').length, 512)
  assert.equal(readEventVpnOverrideOperations(tab, 7, 3_000 + DAY_MS + 1).create, null)

  tab.setItem(STORAGE_KEY, '{not json')
  assert.deepEqual(readEventVpnOverrideOperations(tab, 7, 4_000), { create: null, revokes: [] })
  assert.equal(tab.getItem(STORAGE_KEY), null)
  tab.setItem(STORAGE_KEY, 'x'.repeat(32 * 1024 + 1))
  assert.deepEqual(readEventVpnOverrideOperations(tab, 7, 4_001), { create: null, revokes: [] })
  assert.equal(tab.getItem(STORAGE_KEY), null)

  for (let index = 1; index <= 40; index += 1) {
    retainEventVpnOverrideRevokeOperation(
      tab,
      { gameId: 7, overrideId: uuid(100 + index), expectedPolicyRevision: index },
      null,
      () => uuid(200 + index),
      5_000 + index
    )
  }
  assert.equal(readEventVpnOverrideOperations(tab, 7, 6_000).revokes.length, 32)
  assert.ok((tab.getItem(STORAGE_KEY)?.length ?? 0) <= 32 * 1024)
})

test('admin Event-VPN mutations restore, retain, and terminally clear recovery state', () => {
  const source = readFileSync('src/pages/admin/games/[id]/Info.tsx', 'utf8')
  assert.match(source, /readEventVpnOverrideOperations\(sessionStorage, numId\)/)
  assert.match(source, /retainEventVpnOverrideCreateOperation\(/)
  assert.match(source, /retainEventVpnOverrideRevokeOperation\(/)
  assert.match(source, /expectedPolicyRevision: operation\.expectedPolicyRevision/g)
  assert.match(source, /if \(!isRetryableHttpError\(error\)\)/)
  assert.match(source, /clearEventVpnOverrideCreateOperation\(/)
  assert.match(source, /clearEventVpnOverrideRevokeOperation\(/)
})

test('create recovery clears only the matching terminal owner', () => {
  const tab = storage()
  const operation = retainEventVpnOverrideCreateOperation(
    tab,
    {
      gameId: 7,
      signature: signature('incident response', 15),
      reason: 'incident response',
      durationMinutes: 15,
      expectedPolicyRevision: 1,
    },
    null,
    () => uuid(1),
    7_000
  )
  clearEventVpnOverrideCreateOperation(tab, 7, uuid(2), 7_001)
  assert.equal(readEventVpnOverrideOperations(tab, 7, 7_002).create?.operationId, operation.operationId)
  clearEventVpnOverrideCreateOperation(tab, 7, operation.operationId, 7_003)
  assert.equal(readEventVpnOverrideOperations(tab, 7, 7_004).create, null)
})
