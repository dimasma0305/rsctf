import assert from 'node:assert/strict'
import { test } from 'node:test'
import { getPaginationState } from './PaginationState'

test('pagination always exposes at least one whole page', () => {
  for (const total of [0, -3, Number.NaN, Number.POSITIVE_INFINITY]) {
    assert.deepEqual(getPaginationState(1, total), { page: 1, totalPages: 1 })
  }

  assert.deepEqual(getPaginationState(1, 4.9), { page: 1, totalPages: 4 })
})

test('pagination clamps invalid and stale page values before rendering', () => {
  assert.deepEqual(getPaginationState(0, 4), { page: 1, totalPages: 4 })
  assert.deepEqual(getPaginationState(-2, 4), { page: 1, totalPages: 4 })
  assert.deepEqual(getPaginationState(Number.NaN, 4), { page: 1, totalPages: 4 })
  assert.deepEqual(getPaginationState(3.8, 4), { page: 3, totalPages: 4 })
  assert.deepEqual(getPaginationState(9, 4), { page: 4, totalPages: 4 })
})

test('pagination immediately follows a shrinking result set', () => {
  assert.deepEqual(getPaginationState(4, 1), { page: 1, totalPages: 1 })
})
