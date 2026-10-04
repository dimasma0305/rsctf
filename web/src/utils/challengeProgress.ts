export const visibleChallengeSolveProgress = (solvedCount?: number, challengeCount?: number): number => {
  if (!Number.isFinite(solvedCount) || !Number.isFinite(challengeCount) || (challengeCount ?? 0) <= 0) return 0

  const ratio = Math.max(0, solvedCount ?? 0) / (challengeCount ?? 1)
  return Math.min(100, ratio * 100)
}
