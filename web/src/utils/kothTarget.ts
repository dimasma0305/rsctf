import type { AdHillTarget } from '@Api'

interface KothTargetState {
  cycleNumber: number
  resetPhase: string
}

/**
 * Accept a shared hill address only when it belongs to the lifecycle state the
 * player is looking at. Managed hills are also hidden during reset/readiness so
 * an endpoint cached just before a transition can never be copied as current.
 */
export const selectCurrentKothTarget = (
  target: AdHillTarget | null | undefined,
  state: KothTargetState | null | undefined
): AdHillTarget | null => {
  if (!target || !state || target.cycleNumber !== state.cycleNumber) return null
  if (state.cycleNumber > 0 && state.resetPhase !== 'Active') return null
  return target
}
