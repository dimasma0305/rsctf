export const isKothResetTransition = (phase?: string | null): boolean =>
  phase != null && phase !== 'Active' && phase !== 'Ended'

export const maxKothCooldownTicks = (participants: { remainingTicks: number }[]): number =>
  participants.reduce((remaining, participant) => Math.max(remaining, participant.remainingTicks), 0)

export const kothConfirmationProgress = (current?: number, required?: number): [number, number] => [
  Math.max(0, current ?? 0),
  Math.max(1, required ?? 1),
]
