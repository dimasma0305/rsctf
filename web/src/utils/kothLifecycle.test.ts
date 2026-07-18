import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  isKothResetTransition,
  kothConfirmationProgress,
  maxKothCooldownTicks,
} from './kothLifecycle'

test('only durable reset phases are presented as non-active transitions', () => {
  assert.equal(isKothResetTransition(undefined), false)
  assert.equal(isKothResetTransition('Active'), false)
  assert.equal(isKothResetTransition('Ended'), false)
  assert.equal(isKothResetTransition('Destroying'), true)
  assert.equal(isKothResetTransition('Readiness'), true)
  assert.equal(isKothResetTransition('Failed'), true)
})

test('tied champion cooldown displays the authoritative longest remaining duration', () => {
  assert.equal(maxKothCooldownTicks([]), 0)
  assert.equal(maxKothCooldownTicks([{ remainingTicks: 1 }, { remainingTicks: 2 }]), 2)
})

test('claim progress is non-negative and confirmation always needs at least one tick', () => {
  assert.deepEqual(kothConfirmationProgress(-1, 0), [0, 1])
  assert.deepEqual(kothConfirmationProgress(1, 2), [1, 2])
})
