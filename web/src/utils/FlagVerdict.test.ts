import assert from 'node:assert/strict'
import { test } from 'node:test'
import { flagVerdictReducer, getFlagVerdictKind } from './FlagVerdict'

test('only accepted and wrong answers create cinematic verdicts', () => {
  assert.equal(getFlagVerdictKind('Accepted'), 'success')
  assert.equal(getFlagVerdictKind('WrongAnswer'), 'wrong')

  for (const result of ['FlagSubmitted', 'CheatDetected', 'NotFound', 'UnknownResult']) {
    assert.equal(getFlagVerdictKind(result), null)
  }
})

test('a newer verdict replaces the current one and stale dismissals are ignored', () => {
  const accepted = flagVerdictReducer(null, {
    type: 'show',
    result: 'Accepted',
    sequence: 41,
  })
  assert.deepEqual(accepted, { kind: 'success', sequence: 41 })

  const wrong = flagVerdictReducer(accepted, {
    type: 'show',
    result: 'WrongAnswer',
    sequence: 42,
  })
  assert.deepEqual(wrong, { kind: 'wrong', sequence: 42 })
  assert.equal(flagVerdictReducer(wrong, { type: 'dismiss', sequence: 41 }), wrong)
  assert.equal(flagVerdictReducer(wrong, { type: 'dismiss', sequence: 42 }), null)
})

test('non-cinematic results preserve the current verdict and reset clears it', () => {
  const current = { kind: 'wrong' as const, sequence: 9 }
  assert.equal(
    flagVerdictReducer(current, {
      type: 'show',
      result: 'CheatDetected',
      sequence: 10,
    }),
    current
  )
  assert.equal(flagVerdictReducer(current, { type: 'reset' }), null)
})
