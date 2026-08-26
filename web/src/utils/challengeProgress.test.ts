import assert from 'node:assert/strict'
import test from 'node:test'
import { visibleChallengeSolveProgress } from './challengeProgress'

test('visible challenge solve progress is finite and bounded', () => {
  assert.equal(visibleChallengeSolveProgress(0, 0), 0)
  const beforePermissionChange = visibleChallengeSolveProgress(2, 4)
  assert.equal(beforePermissionChange, 50)
  assert.equal(visibleChallengeSolveProgress(4, 2), 100)
  assert.equal(visibleChallengeSolveProgress(-1, 2), 0)
  assert.equal(visibleChallengeSolveProgress(Number.NaN, 2), 0)
  assert.equal(visibleChallengeSolveProgress(1, Number.NaN), 0)

  // The render calculation consumes each polled response directly, so a
  // division-permission update cannot retain the previous denominator.
  const afterPermissionChange = visibleChallengeSolveProgress(1, 1)
  assert.equal(afterPermissionChange, 100)
})
