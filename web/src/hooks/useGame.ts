import dayjs from 'dayjs'
import { TFunction } from 'i18next'
import useSWR from 'swr'
import { GameStatus } from '@Components/GameCard'
import { OnceSWRConfig } from '@Hooks/useConfig'
import api, { ParticipationStatus } from '@Api'

export const useRecentGames = () => {
  const { data, mutate, error } = api.game.useGameRecentGames(
    { limit: 7 },
    {
      refreshInterval: 30 * 60 * 1000,
    }
  )

  // Guard against SWR hydrating a stale non-array value from persistent
  // cache (e.g. an old 302/HTML response from a misconfigured proxy).
  return { recentGames: Array.isArray(data) ? data : undefined, error, mutate }
}

export const getGameStatus = (game?: { start?: number; end?: number }) => {
  const startTime = dayjs(game?.start)
  const endTime = dayjs(game?.end)

  const total = endTime.diff(startTime, 'minute')
  const current = dayjs().diff(startTime, 'minute')

  const finished = dayjs().isAfter(endTime)
  const started = dayjs().isAfter(startTime)
  const progress = started ? (finished ? 1 : current / total) : 0
  const status = started ? (finished ? GameStatus.Ended : GameStatus.OnGoing) : GameStatus.Coming

  return {
    startTime,
    endTime,
    finished,
    started,
    progress: progress * 100,
    total,
    status,
  }
}

export const toLimitTag = (t: TFunction, limit?: number) => {
  if (!limit || limit === 0) return t('game.tag.multiplayer')
  if (limit === 1) return t('game.tag.individual')
  return t('game.tag.limited', { count: limit })
}

export const useAdminGame = (numId: number) => {
  const { data: game, mutate, error } = api.edit.useEditGetGame(numId, OnceSWRConfig, numId > 0)

  return { game, error, mutate }
}

export const useAdminDivisions = (numId: number) => {
  const { data: divisions, mutate, error } = api.edit.useEditGetDivisions(numId, OnceSWRConfig, numId > 0)

  return { divisions, error, mutate, hasDivisions: (divisions?.length ?? 0) > 0 }
}

export const useGame = (numId: number) => {
  const { data: game, error, mutate } = api.game.useGameGame(numId, OnceSWRConfig, numId > 0)

  return { game, error, mutate, status: game?.status ?? ParticipationStatus.Unsubmitted }
}

export const useGameScoreboard = (numId: number, isTabActive: boolean = true) => {
  const { game } = useGame(numId)
  const { status } = getGameStatus(game)

  const {
    data: scoreboard,
    error,
    mutate,
  } = api.game.useGameScoreboard(numId, {
    ...OnceSWRConfig,
    refreshInterval: status === GameStatus.OnGoing && isTabActive ? 30 * 1000 : 0,
  })

  return { scoreboard, error, mutate }
}

export const useGameTeamInfo = (numId: number, shouldPoll: boolean = true) => {
  const { game } = useGame(numId)
  const { status } = getGameStatus(game)

  const {
    data: teamInfo,
    error,
    mutate,
  } = api.game.useGameChallengesWithTeamInfo(numId, {
    ...OnceSWRConfig,
    shouldRetryOnError: false,
    refreshInterval: status === GameStatus.OnGoing && shouldPoll ? 10 * 1000 : 0,
  })

  return { teamInfo, game, error, mutate }
}

/** A&D — player state poll (own team's containers + flags). Pass doFetch=false
 *  to skip the request entirely (e.g. on pages that only conditionally need it). */
export const useAdState = (numId: number, doFetch: boolean = true) => {
  const { game } = useGame(numId)
  const { status } = getGameStatus(game)
  const {
    data: adState,
    error,
    mutate,
  } = api.game.useGameAdState(
    numId,
    {
      ...OnceSWRConfig,
      shouldRetryOnError: false,
      refreshInterval: status === GameStatus.OnGoing ? 10 * 1000 : 0,
    },
    doFetch
  )
  return { adState, error, mutate }
}

/** Official A&D epoch scoreboard poll. */
export const useAdScoreboard = (numId: number, doFetch: boolean = true) => {
  const { game } = useGame(numId)
  const { status } = getGameStatus(game)
  const {
    data: adScoreboard,
    error,
    mutate,
  } = api.game.useGameAdScoreboard(
    numId,
    {
      ...OnceSWRConfig,
      // Every response has a new generatedAt version, so recursive comparison
      // only scans the full team/service matrix before reaching that difference.
      compare: Object.is,
      // Poll through warmup and post-event closeout. The final request flips
      // fullySettled only after every official epoch is durably materialized.
      refreshInterval: (latest) => {
        if (!doFetch) return 0
        return status === GameStatus.OnGoing || latest?.fullySettled !== true ? 10 * 1000 : 60 * 1000
      },
    },
    doFetch
  )
  return { adScoreboard, error, mutate }
}

/**
 * King of the Hill — dedicated scoreboard poll. Hits the new
 * /api/game/{id}/ad/koth/scoreboard endpoint (not yet in the auto-generated
 * SDK — using useSWR directly for now; swap to api.game.useGameAdKothScoreboard
 * once Api.ts is regenerated).
 */
export interface KothLifecycleFields {
  provisionalClaimantTeamName: string | null
  provisionalClaimantParticipationId: number | null
  provisionalConfirmationTicks: number
  cycleNumber: number
  /** One-based while active; zero while the hill is being reset. */
  cycleTick: number
  resetPhase: KothResetPhase
  isScorable: boolean
  nextResetTicks: number | null
  cooldownParticipants: KothCooldownParticipant[]
}

export interface KothScoreboardHill extends KothLifecycleFields {
  challengeId: number
  title: string
  category: string
  /** Confirmed king only. A claim still proving control is exposed separately. */
  currentHolderTeamName: string | null
  currentHolderParticipationId: number | null
  lastCheckStatus: string | null
}

export type KothResetPhase =
  | 'Active'
  | 'Finalizing'
  | 'Snapshotting'
  | 'Destroying'
  | 'Creating'
  | 'Readiness'
  | 'Activating'
  | 'CooldownRelease'
  | 'Failed'
  | 'Ended'

export interface KothCooldownParticipant {
  participationId: number
  teamName: string
  remainingTicks: number
}

export interface KothHillScore {
  challengeId: number
  /** Weighted average from finalized epochs; this is the ranked value. */
  settledPoints: number
  /** Weighted average including the current, unfinished epoch. */
  projectedPoints: number
  /** Share of eligible token windows in which this team proved control. */
  acquisitionRate: number
  /** Share of scorable checker ticks controlled by this team. */
  controlRate: number
  /** Healthy ticks divided by ticks for which this team was responsible. */
  reliabilityRate: number
  acquisitionWindows: number
  controlledTicks: number
  responsibleTicks: number
  healthyResponsibleTicks: number
  isCurrentHolder: boolean
}
export interface KothEpochScore {
  epoch: number
  points: number
  epochWeight: number
  finalized: boolean
}
export interface KothTeamScoreRow {
  rank: number
  participationId: number
  teamId: number
  teamName: string
  division?: string | null
  settledTotal: number
  projectedTotal: number
  acquisitionRate: number
  controlRate: number
  reliabilityRate: number
  hills: KothHillScore[]
  epochs: KothEpochScore[]
}
export interface KothScoreboardModel {
  epochTicks: number
  cycleTicks: number
  championCooldownTicks: number
  claimConfirmationTicks: number
  startRound: number | null
  started: boolean
  fullySettled: boolean
  currentEpoch: number
  detailEpochLimit: number
  latestRound: number
  /** Unix milliseconds. */
  currentRoundEndsAt: number | null
  tickSeconds: number
  /** Unix milliseconds. */
  generatedAt: number
  isFrozenView: boolean
  /** Unix milliseconds. */
  freeze: number | null
  hills: KothScoreboardHill[]
  teams: KothTeamScoreRow[]
}

export const useKothScoreboard = (numId: number, doFetch: boolean = true) => {
  const { game } = useGame(numId)
  const { status } = getGameStatus(game)
  const {
    data: kothScoreboard,
    error,
    mutate,
  } = useSWR<KothScoreboardModel>(doFetch && numId > 0 ? `/api/game/${numId}/ad/koth/scoreboard` : null, {
    ...OnceSWRConfig,
    compare: Object.is,
    // Keep polling through event closeout until the final partial epoch has
    // been durably settled; after that, only refresh occasionally.
    refreshInterval: (latest) => {
      if (!doFetch) return 0
      return status === GameStatus.OnGoing || latest?.fullySettled !== true ? 10 * 1000 : 60 * 1000
    },
  })
  return { kothScoreboard, error, mutate }
}

/** A&D — team API token hint (never plaintext); used by the per-challenge modal. */
export const useAdTokenHint = (numId: number, doFetch: boolean = true) => {
  const {
    data: adTokenHint,
    error,
    mutate,
  } = api.game.useGameAdTokenHint(
    numId,
    {
      ...OnceSWRConfig,
      shouldRetryOnError: false,
    },
    doFetch
  )
  return { adTokenHint, error, mutate }
}

/** A&D admin — operator console state poll. Faster refresh during active games. */
export const useAdminAdState = (numId: number) => {
  const { game } = useGame(numId)
  const { status } = getGameStatus(game)
  const {
    data: adminAdState,
    error,
    mutate,
  } = api.edit.useEditAdState(numId, {
    ...OnceSWRConfig,
    refreshInterval: status === GameStatus.OnGoing ? 5 * 1000 : 0,
  })
  return { adminAdState, error, mutate }
}

/** One KotH hill in the operator console — the shared container + its current king + verdict. */
export interface AdminKothHill extends KothLifecycleFields {
  challengeId: number
  title: string
  isEnabled: boolean
  containerGuid: string | null
  containerIp: string | null
  containerPort: number | null
  lastCheckStatus: string | null
  currentHolderTeamName: string | null
  currentHolderParticipationId: number | null
  /** Exact persisted state-machine phase (for example, CreatePending). */
  durablePhase: string
  cycleChampions: KothCycleChampion[]
  oldContainerId: string | null
  replacementContainerId: string | null
  resetAttempt: number
  readinessFailureCount: number
  lastReadinessError: string | null
  canRetry: boolean
  resetReceiptId: number | null
  scoringReceiptId: number | null
}

export interface KothCycleChampion {
  sourceCycleNumber: number
  participationId: number
  teamName: string
  healthyControlledTicks: number
}
export interface AdminKothStateModel {
  epochTicks: number
  cycleTicks: number
  championCooldownTicks: number
  claimConfirmationTicks: number
  tickSeconds: number
  hills: AdminKothHill[]
  teams: KothTeamScoreRow[]
}

export interface AdminKothAuditReceipt {
  id: number
  phase: string
  attempt: number
  receipt: unknown
  filesystemDiff: unknown | null
  /** Unix milliseconds. */
  createdAt: number
}

export interface AdminKothReceiptsModel {
  challengeId: number
  cycleNumber: number
  receipts: AdminKothAuditReceipt[]
}

/**
 * KotH admin — operator console state poll (the KotH analogue of
 * {@link useAdminAdState}). Hits the new /api/edit/games/{id}/ad/koth/state
 * endpoint directly via useSWR (same pattern as {@link useKothScoreboard} —
 * not yet in the auto-generated SDK). Always resolves to an object (empty
 * hills for games with no KotH challenges), so callers can branch on
 * `hills.length` without a separate loading guard.
 */
export const useAdminKothState = (numId: number) => {
  const { game } = useGame(numId)
  const { status } = getGameStatus(game)
  const {
    data: adminKothState,
    error,
    mutate,
  } = useSWR<AdminKothStateModel>(numId > 0 ? `/api/edit/games/${numId}/ad/koth/state` : null, {
    ...OnceSWRConfig,
    shouldRetryOnError: false,
    refreshInterval: status === GameStatus.OnGoing ? 5 * 1000 : 0,
  })
  return { adminKothState, error, mutate }
}
