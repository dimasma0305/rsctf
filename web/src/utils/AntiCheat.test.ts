import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  CHEAT_REPORT_REFRESH_INTERVAL_MS,
  CHEAT_REPORT_STALE_AFTER_MS,
  antiCheatExemptionState,
  evidenceContribution,
  hasActiveAntiCheatExemption,
  isCheatReportStale,
  normalizeCheatViewTab,
} from './AntiCheat'

test('anti-cheat monitor normalizes unsupported URL tabs and refreshes at a bounded cadence', () => {
  assert.equal(normalizeCheatViewTab('submissions'), 'submissions')
  assert.equal(normalizeCheatViewTab('analysis'), 'analysis')
  assert.equal(normalizeCheatViewTab('unknown'), 'analysis')
  assert.equal(normalizeCheatViewTab(null), 'analysis')
  assert.equal(CHEAT_REPORT_REFRESH_INTERVAL_MS, 60_000)
  assert.equal(CHEAT_REPORT_STALE_AFTER_MS, 180_000)
})

test('anti-cheat report freshness uses the persisted reconciliation watermark', () => {
  const now = 1_000_000
  assert.equal(isCheatReportStale(undefined, now), true)
  assert.equal(isCheatReportStale(null, now), true)
  assert.equal(isCheatReportStale(Number.NaN, now), true)
  assert.equal(isCheatReportStale(now - CHEAT_REPORT_STALE_AFTER_MS, now), false)
  assert.equal(isCheatReportStale(now - CHEAT_REPORT_STALE_AFTER_MS - 1, now), true)
})

test('evidence contribution distinguishes raw weight from points that actually count', () => {
  assert.equal(evidenceContribution({ counted: true, scoreDelta: 80 }), 80)
  assert.equal(evidenceContribution({ counted: true, scoreDelta: 80, appliedDelta: 25 }), 25)
  assert.equal(evidenceContribution({ counted: false, scoreDelta: 80 }), 0)
  assert.equal(evidenceContribution({ counted: true }), 0)
})

test('anti-cheat exemptions are active only until their exact expiry', () => {
  const now = 1_000
  assert.equal(hasActiveAntiCheatExemption({ exemptionExpiresAtUtc: now + 1 }, now), true)
  assert.equal(hasActiveAntiCheatExemption({ exemptionExpiresAtUtc: now }, now), false)
  assert.equal(hasActiveAntiCheatExemption({}, now), false)
  assert.equal(antiCheatExemptionState({}, now), 'unreviewed')
  assert.equal(antiCheatExemptionState({ exemptionExpiresAtUtc: now + 1 }, now), 'active')
  assert.equal(antiCheatExemptionState({ adjudicatedAtUtc: 500, exemptionExpiresAtUtc: now }, now), 'expired')
})
