export type ScoreboardPageRequest = {
  skip: number
  count: number
  divisionId: number | null
  search: string
  locateTeam?: string
}

export const scoreboardPagePath = (gameId: number, request: ScoreboardPageRequest) => {
  const query = new URLSearchParams({ skip: String(request.skip), count: String(request.count) })
  if (request.divisionId !== null) query.set('divisionId', String(request.divisionId))
  if (request.search.trim()) query.set('search', request.search.trim())
  if (request.locateTeam) query.set('locateTeam', request.locateTeam)
  return `/api/game/${gameId}/scoreboard/page?${query}`
}

export interface ScoreboardPaging {
  page: number
  skip: number
  count: number
  total: number
  pending: boolean
  keyword: string
  setKeyword: (keyword: string) => void
  setPage: (page: number) => void
  findTeam: (name: string) => void
}
