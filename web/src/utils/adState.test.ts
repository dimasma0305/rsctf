import assert from 'node:assert/strict'
import test from 'node:test'
import { adRoundSecondsRemaining } from './adState'

test('A&D player countdown freezes at the operator pause instant', () => {
  const end = Date.UTC(2026, 6, 25, 12, 1, 0)
  const pausedAt = Date.UTC(2026, 6, 25, 12, 0, 20)
  const muchLater = Date.UTC(2026, 6, 25, 12, 10, 0)

  assert.equal(adRoundSecondsRemaining(end, muchLater, true, pausedAt), 40)
  assert.equal(adRoundSecondsRemaining(end, muchLater, false, null), 0)
})

test('A&D player countdown handles warmup and malformed timestamps safely', () => {
  assert.equal(adRoundSecondsRemaining(null, Date.now(), false), null)
  assert.equal(adRoundSecondsRemaining('not-a-time', Date.now(), false), null)
})
