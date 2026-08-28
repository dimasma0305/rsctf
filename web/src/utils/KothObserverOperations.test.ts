import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import type { AdminKothObserverModel } from '../hooks/useGame'
import {
  type KothObserverOperationOwner,
  newKothObserverOperationId,
  ownsKothObserverResult,
} from './KothObserverOperations'

const owner = (operationId: string, generation: number): KothObserverOperationOwner => ({
  challengeId: 9,
  expectedRevision: generation,
  generation,
  operationId,
  kind: 'Rotate',
  viewGeneration: 4,
})

const result = (operationId: string, revision: number): AdminKothObserverModel => ({
  challengeId: 9,
  revision,
  operationId,
  claimSource: 'Api',
  configured: true,
  managedTargetReporting: true,
  secretHint: null,
  objectiveCount: null,
  objectiveIds: null,
  objectiveSchemaHash: null,
  createdAt: null,
  rotatedAt: null,
  lastUsedAt: null,
  lastObservationAt: null,
  contextPath: '/context',
  observationPath: '/observations',
})

test('mutation identities are opaque random UUIDs', () => {
  const ids = Array.from({ length: 64 }, newKothObserverOperationId)
  assert.equal(new Set(ids).size, ids.length)
  for (const id of ids) {
    assert.match(id, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/)
  }
})

test('an older reversed response cannot replace the current observer mutation', () => {
  const current = owner('00000000-0000-4000-8000-000000000002', 2)
  assert.equal(ownsKothObserverResult(current, result('00000000-0000-4000-8000-000000000001', 2), 9, 4), false)
  assert.equal(ownsKothObserverResult(current, result(current.operationId, 3), 9, 4), true)
})

test('closing or switching the observer view fences a late one-time result', () => {
  const current = owner('00000000-0000-4000-8000-000000000003', 3)
  assert.equal(ownsKothObserverResult(current, result(current.operationId, 4), null, 5), false)
  assert.equal(ownsKothObserverResult(current, result(current.operationId, 4), 10, 4), false)
})

test('a malformed revision cannot satisfy the operation owner', () => {
  const current = owner('00000000-0000-4000-8000-000000000004', 4)
  assert.equal(ownsKothObserverResult(current, result(current.operationId, 4), 9, 4), false)
  assert.equal(ownsKothObserverResult(current, result(current.operationId, 6), 9, 4), false)
})

test('the operator panel owns one ref-backed operation and recovers before retrying', () => {
  const source = readFileSync('src/components/admin/KothOpsPanel.tsx', 'utf8')
  assert.match(source, /observerMutationRef = useRef<KothObserverOperationOwner \| null>/)
  assert.match(source, /observerBusyRef = useRef\(false\)/)
  assert.match(source, /recoverObserverOperation\(operation\)/)
  assert.match(source, /result = await requestObserverOperation\(operation\)/)
  assert.match(source, /ownsKothObserverResult/)
})
