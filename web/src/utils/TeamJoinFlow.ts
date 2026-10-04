interface TeamJoinAttempt {
  accept: () => Promise<void>
  onAccepted: () => void
  onRejected: (error: unknown) => void
}

export const settleTeamJoinAttempt = async ({ accept, onAccepted, onRejected }: TeamJoinAttempt) => {
  try {
    await accept()
  } catch (error) {
    onRejected(error)
    return false
  }

  onAccepted()
  return true
}
