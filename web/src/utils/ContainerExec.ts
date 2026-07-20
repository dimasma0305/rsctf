/**
 * Select the platform Admin terminal or the narrower per-game organizer path.
 * Reject invalid ids locally so the modal never falls back to the global hub.
 */
export const containerExecHubPath = (scopedGameId?: number): string => {
  if (scopedGameId === undefined) return '/hub/containerExec'
  if (!Number.isSafeInteger(scopedGameId) || scopedGameId <= 0) {
    throw new Error('scoped container exec requires a positive game id')
  }
  return `/hub/containerExec/games/${scopedGameId}`
}
