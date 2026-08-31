import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { RetryableMutationOwner } from './RetryableMutationOwner'

test('retryable create intent never survives an account or browser-session boundary', () => {
  const source = readFileSync(new URL('./RetryableMutationOwner.ts', import.meta.url), 'utf8')
  assert.doesNotMatch(source, /sessionStorage|localStorage|indexedDB/)
})

test('lifecycle create owners cannot restore an operation from another authenticated account', () => {
  for (const relativePath of [
    '../components/TeamCreateModal.tsx',
    '../components/admin/ChallengeCreateModal.tsx',
    '../components/admin/GameCreateModal.tsx',
    '../pages/posts/[postId]/Edit.tsx',
  ]) {
    const source = readFileSync(new URL(relativePath, import.meta.url), 'utf8')
    assert.match(source, /RetryableMutationOwner/)
    assert.doesNotMatch(source, /RetryableOperationKey|sessionStorage|localStorage|indexedDB/)
  }
})

test('a mutation owner synchronously excludes duplicate activation and retains an ambiguous identity', () => {
  let sequence = 0
  const owner = new RetryableMutationOwner(() => `operation-${++sequence}`)
  const first = owner.claim('same')!
  assert.equal(owner.claim('same'), null)
  assert.equal(owner.settle(first, false), true)
  const retry = owner.claim('same')!
  assert.equal(retry.operationId, first.operationId)
  assert.equal(retry.generation, first.generation + 1)
})

test('completion, changed input, and cancellation rotate or fence results', () => {
  let sequence = 0
  const owner = new RetryableMutationOwner(() => `operation-${++sequence}`)
  const first = owner.claim('first')!
  owner.cancel()
  assert.equal(owner.owns(first), false)
  const second = owner.claim('first')!
  assert.notEqual(second.operationId, first.operationId)
  assert.equal(owner.settle(second, true), true)
  const third = owner.claim('second')!
  assert.notEqual(third.operationId, second.operationId)
})
