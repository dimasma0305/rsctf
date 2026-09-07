import { ChallengeBuildStatus, type ChallengeBuildAuditModel } from '@Api'

export const matchesBuildQuery = (build: ChallengeBuildAuditModel, query: string) => {
  const needle = query.trim().toLocaleLowerCase()
  return (
    !needle ||
    [build.challengeTitle, build.challengeId, build.gameId, build.imageRef, build.kind, build.trigger]
      .filter((value) => value != null)
      .join(' ')
      .toLocaleLowerCase()
      .includes(needle)
  )
}

export const BUILD_STATUS_COLOR: Record<ChallengeBuildStatus, string> = {
  None: 'gray',
  Success: 'teal',
  Failed: 'red',
  Building: 'yellow',
  NotApplicable: 'gray',
  Queued: 'blue',
  MissingDockerfile: 'orange',
}

export const BUILD_STATUS_VARIANT = 'light' as const

export const formatBuildDuration = (milliseconds: number) => {
  if (!milliseconds) return '—'
  if (milliseconds < 1000) return `${milliseconds}ms`
  if (milliseconds < 60_000) return `${(milliseconds / 1000).toFixed(1)}s`
  return `${Math.floor(milliseconds / 60_000)}m ${Math.floor((milliseconds % 60_000) / 1000)}s`
}
