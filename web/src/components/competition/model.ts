import { ChallengeType, SubmissionType, type ChallengeInfo, type ChallengeItem } from '@Api'

export type ChallengeView = 'globe' | 'list' | 'cards'
export type ChallengeSort = 'name' | 'score' | 'solves'

export const resolveChallengeView = (value: unknown, compact: boolean): ChallengeView =>
  value === 'globe' || value === 'list' || value === 'cards' ? value : compact ? 'list' : 'globe'

export const isLiveChallenge = (challenge: Pick<ChallengeInfo, 'type'>) =>
  challenge.type === ChallengeType.AttackDefense || challenge.type === ChallengeType.KingOfTheHill

export const isAcceptedSolve = (solve: Pick<ChallengeItem, 'type'> | undefined) =>
  solve !== undefined && solve.type !== SubmissionType.Unaccepted

export const sortChallenges = (challenges: readonly ChallengeInfo[], sort: ChallengeSort) =>
  [...challenges].sort((a, b) => {
    // Live-engine challenges have no comparable static point or solve total.
    if (sort !== 'name') {
      const liveDifference = Number(isLiveChallenge(a)) - Number(isLiveChallenge(b))
      if (liveDifference) return liveDifference
      if (!isLiveChallenge(a)) {
        const difference = sort === 'score' ? b.score - a.score : b.solved - a.solved
        if (difference) return difference
      }
    }
    return a.title.localeCompare(b.title) || a.id - b.id
  })

export const challengePage = <T>(items: readonly T[], page: number, size: number) => {
  const pageSize = Math.max(1, Math.floor(size) || 1)
  const pages = Math.max(1, Math.ceil(items.length / pageSize))
  const current = Math.min(pages, Math.max(1, Math.floor(page) || 1))
  return { current, pages, items: items.slice((current - 1) * pageSize, current * pageSize) }
}

export const projectSpherePoint = (x: number, y: number, z: number, yaw: number, pitch: number) => {
  const rotatedX = x * Math.cos(yaw) + z * Math.sin(yaw)
  const rotatedZ = z * Math.cos(yaw) - x * Math.sin(yaw)
  return {
    x: rotatedX,
    y: y * Math.cos(pitch) - rotatedZ * Math.sin(pitch),
    z: y * Math.sin(pitch) + rotatedZ * Math.cos(pitch),
  }
}
