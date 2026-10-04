export interface ArenaLiveRoutes {
  adScoreboard: string
  kothScoreboard: string
  scoreboard: string
  game: string
}

export const arenaLiveRoutes = (gameId: string | number): ArenaLiveRoutes => {
  const encoded = encodeURIComponent(String(gameId))
  return {
    adScoreboard: `/api/game/${encoded}/ad/scoreboard`,
    kothScoreboard: `/api/game/${encoded}/ad/koth/scoreboard`,
    scoreboard: `/api/game/${encoded}/scoreboard`,
    game: `/api/game/${encoded}`,
  }
}

const MAX_RETRY_AFTER_MS = 60_000

export class ArenaHttpError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly retryAfterMs: number | null
  ) {
    super(message)
  }
}

export const parseArenaRetryAfter = (value: string | null, now = Date.now()): number | null => {
  if (!value) return null
  const seconds = Number(value)
  const milliseconds = Number.isFinite(seconds) ? seconds * 1_000 : Date.parse(value) - now
  if (!Number.isFinite(milliseconds) || milliseconds < 0) return null
  return Math.min(milliseconds, MAX_RETRY_AFTER_MS)
}

export const arenaPollDelay = (
  failures: number,
  retryAfterMs: number | null,
  random = Math.random
): number => {
  if (retryAfterMs !== null) return retryAfterMs
  if (failures <= 0) return 12_000 + Math.floor(random() * 6_001)
  const ceiling = Math.min(60_000, 1_000 * 2 ** Math.min(failures, 6))
  return Math.max(750, Math.floor(random() * ceiling))
}

export const arenaReconnectDelay = (failures: number, random = Math.random): number => {
  const ceiling = Math.min(60_000, 1_000 * 2 ** Math.min(Math.max(failures, 1), 6))
  return Math.max(500, Math.floor(random() * ceiling))
}

export interface ArenaRosterSeed {
  key: string
  participationId?: number
  teamName: string
  ad?: any
  koth?: any
  jeopardy?: any
}

/** Union every official board. A suspended team disappears only when it is absent
 * from all authoritative boards, while late KotH/A&D admission can join an
 * already-mounted arena without a reload. */
export const mergeArenaRoster = (adRows: any[], kothRows: any[], jeopardyRows: any[]): ArenaRosterSeed[] => {
  const byName = new Map<string, ArenaRosterSeed>()
  for (const row of adRows) {
    byName.set(row.teamName, {
      key: `p${row.participationId}`,
      participationId: row.participationId,
      teamName: row.teamName,
      ad: row,
    })
  }
  for (const row of kothRows) {
    const current = byName.get(row.teamName)
    if (current) current.koth = row
    else
      byName.set(row.teamName, {
        key: `p${row.participationId}`,
        participationId: row.participationId,
        teamName: row.teamName,
        koth: row,
      })
  }
  for (const row of jeopardyRows) {
    const current = byName.get(row.name)
    if (current) current.jeopardy = row
    else byName.set(row.name, { key: `j${row.id}`, teamName: row.name, jeopardy: row })
  }
  return [...byName.values()].sort((a, b) => a.key.localeCompare(b.key))
}
