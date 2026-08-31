import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { challengeRevision, prepareChallengeMutation } from './ChallengeMutation'

test('challenge mutation keeps its operation ID for an identical retry', () => {
  let sequence = 0
  const createId = () => `operation-${++sequence}`
  const first = prepareChallengeMutation({ isEnabled: true }, 7, null, createId)
  const retry = prepareChallengeMutation({ isEnabled: true }, 7, first.operation, createId)

  assert.equal(first.payload.operationId, 'operation-1')
  assert.equal(first.payload.expectedRevision, 7)
  assert.equal(retry.operation, first.operation)
  assert.deepEqual(retry.payload, first.payload)
})

test('challenge mutation rotates its operation ID after the intent or revision changes', () => {
  let sequence = 0
  const createId = () => `operation-${++sequence}`
  const first = prepareChallengeMutation({ isEnabled: true }, 7, null, createId)
  const changedIntent = prepareChallengeMutation({ isEnabled: false }, 7, first.operation, createId)
  const changedRevision = prepareChallengeMutation({ isEnabled: false }, 8, changedIntent.operation, createId)

  assert.equal(changedIntent.payload.operationId, 'operation-2')
  assert.equal(changedRevision.payload.operationId, 'operation-3')
  assert.equal(changedRevision.payload.expectedRevision, 8)
})

test('challenge mutation strips server-owned operation fields and supports older responses', () => {
  const prepared = prepareChallengeMutation(
    { title: 'new title', operationId: 'server-value', expectedRevision: 99 },
    undefined,
    null,
    () => 'operation-1'
  )

  assert.deepEqual(prepared.payload, { title: 'new title', operationId: 'operation-1' })
  assert.equal(challengeRevision({ revision: 4 }), 4)
  assert.equal(challengeRevision({ revision: 4.5 }), undefined)
  assert.equal(challengeRevision({}), undefined)
})

test('every ordinary challenge create and update call sends a prepared operation', () => {
  const files = [
    'src/components/admin/ChallengeCreateModal.tsx',
    'src/pages/admin/games/[id]/challenges/Index.tsx',
    'src/pages/admin/games/[id]/challenges/[chalId]/Index.tsx',
    'src/pages/admin/games/[id]/challenges/[chalId]/Flags.tsx',
  ]
  const source = files.map((file) => readFileSync(file, 'utf8')).join('\n')

  assert.equal((source.match(/editAddGameChallenge\(/g) ?? []).length, 1)
  assert.equal((source.match(/editUpdateGameChallenge\(/g) ?? []).length, 3)
  assert.equal((source.match(/editAddGameChallenge\([^\n]*prepared\.payload\)/g) ?? []).length, 1)
  assert.equal((source.match(/editUpdateGameChallenge\([^\n]*prepared\.payload\)/g) ?? []).length, 3)
})
