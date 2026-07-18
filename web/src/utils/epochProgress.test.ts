import assert from 'node:assert/strict'
import { test } from 'node:test'
import { epochProgress } from './epochProgress'

test('warmup and malformed settings do not claim epoch progress', () => {
  assert.equal(epochProgress(4, null, 8), null)
  assert.equal(epochProgress(4, 5, 8), null)
  assert.equal(epochProgress(4, -1, 8), null)
  assert.equal(epochProgress(5, 5, 0), null)
  assert.equal(epochProgress(5.5, 5, 8), null)
})

test('first and final rounds stay within the official epoch', () => {
  assert.deepEqual(epochProgress(5, 5, 8), {
    epoch: 1,
    tick: 1,
    totalTicks: 8,
  })
  assert.deepEqual(epochProgress(12, 5, 8), {
    epoch: 1,
    tick: 8,
    totalTicks: 8,
  })
})

test('progress rolls into the next epoch without changing the absolute round', () => {
  assert.deepEqual(epochProgress(13, 5, 8), {
    epoch: 2,
    tick: 1,
    totalTicks: 8,
  })
})
