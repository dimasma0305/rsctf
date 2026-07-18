import assert from 'node:assert/strict'
import test from 'node:test'
import type { AdHillTarget } from '../Api'
import { selectCurrentKothTarget } from './kothTarget'

const target = (cycleNumber: number): AdHillTarget => ({
  ip: '10.40.0.12',
  port: 8080,
  cycleNumber,
  lastCheckStatus: 'Ok',
  lastRefreshRound: 7,
})

test('a target is shown only for the exact lifecycle cycle', () => {
  assert.equal(selectCurrentKothTarget(target(4), { cycleNumber: 4, resetPhase: 'Active' })?.ip, '10.40.0.12')
  assert.equal(selectCurrentKothTarget(target(3), { cycleNumber: 4, resetPhase: 'Active' }), null)
  assert.equal(selectCurrentKothTarget(target(4), null), null)
})

test('managed targets fail closed during reset and readiness', () => {
  for (const resetPhase of ['Destroying', 'Creating', 'Readiness', 'Failed', 'Ended']) {
    assert.equal(selectCurrentKothTarget(target(4), { cycleNumber: 4, resetPhase }), null)
  }
})

test('an external pre-cycle target remains available', () => {
  assert.equal(selectCurrentKothTarget(target(0), { cycleNumber: 0, resetPhase: 'Readiness' })?.ip, '10.40.0.12')
})
