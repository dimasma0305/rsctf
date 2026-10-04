import type { AdTeamCellModel, AdTeamRowModel } from '@Api'

export type AdOpsHealthFilter = '' | 'attention' | 'Ok' | 'Mumble' | 'Offline' | 'InternalError' | 'unchecked'

export const matchesAdOpsHealth = (cell: AdTeamCellModel | undefined, filter: AdOpsHealthFilter) => {
  if (!filter) return true
  if (filter === 'attention') return cell?.lastCheckStatus !== 'Ok'
  if (filter === 'unchecked') return !cell?.lastCheckStatus
  return cell?.lastCheckStatus === filter
}

// Team filters only inspect the loaded snapshot. Never issue one read per cell.
export const filterAdOpsTeams = (
  teams: AdTeamRowModel[],
  challengeIds: number[],
  search: string,
  health: AdOpsHealthFilter
) => {
  const query = search.trim().toLowerCase()
  return teams.filter(
    (team) =>
      team.teamName.toLowerCase().includes(query) &&
      (!health ||
        challengeIds.some((id) =>
          matchesAdOpsHealth(
            team.services.find((cell) => cell.challengeId === id),
            health
          )
        ))
  )
}
