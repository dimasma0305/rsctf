export interface EpochProgress {
  epoch: number
  tick: number
  totalTicks: number
}

/** Locate a scoring round within its snapshotted official epoch. */
export const epochProgress = (
  currentRound: number,
  startRound: number | null | undefined,
  epochTicks: number
): EpochProgress | null => {
  if (
    startRound == null ||
    !Number.isInteger(currentRound) ||
    !Number.isInteger(startRound) ||
    !Number.isInteger(epochTicks) ||
    startRound <= 0 ||
    currentRound < startRound ||
    epochTicks <= 0
  ) {
    return null
  }

  const offset = currentRound - startRound
  return {
    epoch: Math.floor(offset / epochTicks) + 1,
    tick: (offset % epochTicks) + 1,
    totalTicks: epochTicks,
  }
}
