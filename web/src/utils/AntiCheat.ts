export const CHEAT_REPORT_REFRESH_INTERVAL_MS = 60_000
export const CHEAT_REPORT_STALE_AFTER_MS = CHEAT_REPORT_REFRESH_INTERVAL_MS * 3

export interface CheatReportFreshness {
  sealedAt?: number | null
  lastReconciledAt?: number | null
  pendingJobs?: number
  oldestPendingAt?: number | null
  reconciliationPending?: boolean
}

const overdue = (at: number | null | undefined, now: number) =>
  at == null || !Number.isFinite(at) || now - at > CHEAT_REPORT_STALE_AFTER_MS

/**
 * A report is stale only while evidence it has not evaluated has been waiting
 * too long. A sealed report is final, and a quiet event has nothing waiting,
 * so an old reconciliation time alone is not staleness.
 */
export const isCheatReportStale = (report: CheatReportFreshness | undefined, now: number = Date.now()): boolean => {
  if (!report || report.sealedAt != null) return false
  if ((report.pendingJobs ?? 0) > 0 && overdue(report.oldestPendingAt, now)) return true
  return report.reconciliationPending === true && overdue(report.lastReconciledAt, now)
}

export type CheatViewTab = 'analysis' | 'submissions'

export const normalizeCheatViewTab = (value: string | null): CheatViewTab =>
  value === 'submissions' ? 'submissions' : 'analysis'

export interface EvidenceContribution {
  counted?: boolean
  scoreDelta?: number
  appliedDelta?: number
}

export const evidenceContribution = ({ counted, scoreDelta, appliedDelta }: EvidenceContribution): number =>
  appliedDelta ?? (counted ? (scoreDelta ?? 0) : 0)

export interface ExemptionWindow {
  exemptionExpiresAtUtc?: number | null
  adjudicatedAtUtc?: number | null
}

export const hasActiveAntiCheatExemption = (block: ExemptionWindow, now: number = Date.now()): boolean =>
  (block.exemptionExpiresAtUtc ?? 0) > now

export type AntiCheatExemptionState = 'unreviewed' | 'active' | 'expired'

export const antiCheatExemptionState = (block: ExemptionWindow, now: number = Date.now()): AntiCheatExemptionState => {
  if (hasActiveAntiCheatExemption(block, now)) return 'active'
  return block.adjudicatedAtUtc != null || block.exemptionExpiresAtUtc != null ? 'expired' : 'unreviewed'
}
