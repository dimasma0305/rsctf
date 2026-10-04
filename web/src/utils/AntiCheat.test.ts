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

test('anti-cheat report freshness is based on waiting work, never on a sealed or quiet report', () => {
  const now = 1_000_000
  const old = now - CHEAT_REPORT_STALE_AFTER_MS - 1
  const recent = now - CHEAT_REPORT_STALE_AFTER_MS
  assert.equal(isCheatReportStale(undefined, now), false)
  // A finished event keeps its final report; it never turns stale.
  assert.equal(isCheatReportStale({ sealedAt: 1, lastReconciledAt: 1, reconciliationPending: true }, now), false)
  // A quiet live event has nothing to evaluate.
  assert.equal(isCheatReportStale({ lastReconciledAt: old, pendingJobs: 0 }, now), false)
  assert.equal(isCheatReportStale({ lastReconciledAt: null, reconciliationPending: false }, now), false)
  // Unapplied evidence waits on a reconciler that has not run recently.
  assert.equal(isCheatReportStale({ lastReconciledAt: old, reconciliationPending: true }, now), true)
  assert.equal(isCheatReportStale({ lastReconciledAt: recent, reconciliationPending: true }, now), false)
  assert.equal(isCheatReportStale({ lastReconciledAt: Number.NaN, reconciliationPending: true }, now), true)
  // Evaluation jobs waiting past the threshold.
  assert.equal(isCheatReportStale({ lastReconciledAt: recent, pendingJobs: 2, oldestPendingAt: old }, now), true)
  assert.equal(isCheatReportStale({ lastReconciledAt: old, pendingJobs: 2, oldestPendingAt: recent }, now), false)
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
