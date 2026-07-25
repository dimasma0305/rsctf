export const adRoundSecondsRemaining = (
  roundEndsAt: number | string | null | undefined,
  nowMs: number,
  scoringPaused: boolean,
  scoringPausedAt?: number | string | null
): number | null => {
  if (roundEndsAt === null || roundEndsAt === undefined) return null
  const endMs = new Date(roundEndsAt).getTime()
  const pausedMs =
    scoringPaused && scoringPausedAt !== null && scoringPausedAt !== undefined
      ? new Date(scoringPausedAt).getTime()
      : nowMs
  if (!Number.isFinite(endMs) || !Number.isFinite(pausedMs)) return null
  return Math.max(0, Math.floor((endMs - pausedMs) / 1000))
}
