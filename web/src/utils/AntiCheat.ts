export const CHEAT_REPORT_REFRESH_INTERVAL_MS = 60_000
export const CHEAT_REPORT_STALE_AFTER_MS = CHEAT_REPORT_REFRESH_INTERVAL_MS * 3

export const isCheatReportStale = (
  lastReconciledAt: number | null | undefined,
  now: number = Date.now()
): boolean =>
  lastReconciledAt == null ||
  !Number.isFinite(lastReconciledAt) ||
  now - lastReconciledAt > CHEAT_REPORT_STALE_AFTER_MS

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
