export interface ArenaRoutes {
  game: string
  standardScoreboard: string
  combinedScoreboard: string
  adScoreboard: string
  kothScoreboard: string
}

/** Keep the arena on the exact case-sensitive routes registered by the game routers. */
export const arenaRoutes = (gameId: string | number): ArenaRoutes => {
  const id = encodeURIComponent(String(gameId))
  return {
    game: `/api/game/${id}`,
    standardScoreboard: `/api/game/${id}/scoreboard`,
    combinedScoreboard: `/api/game/${id}/scoreboard/combined`,
    // A&D retains its established generated-API contract; the other game
    // surfaces are registered under the canonical lowercase game router.
    adScoreboard: `/api/Game/${id}/Ad/Scoreboard`,
    kothScoreboard: `/api/game/${id}/ad/koth/scoreboard`,
  }
}

export interface ArenaMatchTiming {
  endTime: number | null
  serverOffset: number
}

export interface ArenaGameTimingSample {
  end?: unknown
  serverTime?: unknown
}

export interface ArenaFinalBoard {
  fullySettled?: boolean
  items?: unknown[]
  modes?: {
    jeopardy?: { active?: boolean }
    attackDefense?: { active?: boolean }
    koth?: { active?: boolean }
  }
}

export type ArenaFinalState = 'playing' | 'settling' | 'podium'

export const initialArenaMatchTiming = (): ArenaMatchTiming => ({ endTime: null, serverOffset: 0 })

const positiveTimestamp = (value: unknown): number | null =>
  typeof value === 'number' && Number.isFinite(value) && value > 0 ? value : null

/**
 * Apply one response-owned game timing sample.
 *
 * The game endpoint stamps `serverTime` beside `end`, so client wall-clock skew
 * cannot finish the arena early. A malformed or partial response preserves the
 * last authoritative value instead of erasing the countdown.
 */
export const observeArenaGameTiming = (
  current: ArenaMatchTiming,
  sample: ArenaGameTimingSample,
  receivedAt: number = Date.now()
): ArenaMatchTiming => {
  const endTime = positiveTimestamp(sample.end) ?? current.endTime
  const serverTime = positiveTimestamp(sample.serverTime)
  const serverOffset =
    serverTime === null || !Number.isFinite(receivedAt) ? current.serverOffset : serverTime - receivedAt
  return { endTime, serverOffset }
}

/** Set a local-only deadline for the self-contained preview arena. */
export const previewArenaMatchTiming = (endTime: number): ArenaMatchTiming => ({ endTime, serverOffset: 0 })

export const arenaServerNow = (timing: ArenaMatchTiming, localNow: number = Date.now()) =>
  localNow + timing.serverOffset

export const arenaSecondsRemaining = (timing: ArenaMatchTiming, localNow: number = Date.now()) =>
  timing.endTime === null ? 0 : Math.max(0, Math.ceil((timing.endTime - arenaServerNow(timing, localNow)) / 1000))

export const arenaHasEnded = (timing: ArenaMatchTiming, localNow: number = Date.now()) =>
  timing.endTime !== null && arenaServerNow(timing, localNow) >= timing.endTime

/**
 * Final podiums use the Overall board because it is the authoritative ranking
 * for pure and mixed Jeopardy, A&D, and KotH events. Epoch formats must finish
 * their durable closeout before the podium is allowed to appear.
 */
export const resolveArenaFinalState = (
  timing: ArenaMatchTiming,
  board: ArenaFinalBoard | null,
  localNow: number = Date.now()
): ArenaFinalState => {
  if (!arenaHasEnded(timing, localNow)) return 'playing'
  const hasActiveFormat =
    board?.modes != null &&
    [board.modes.jeopardy, board.modes.attackDefense, board.modes.koth].some((mode) => mode?.active === true)
  if (!board?.fullySettled || !hasActiveFormat || !Array.isArray(board.items) || board.items.length === 0)
    return 'settling'
  return 'podium'
}
