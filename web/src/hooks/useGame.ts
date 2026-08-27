import dayjs, { Dayjs } from 'dayjs'
import { TFunction } from 'i18next'
import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import useSWR, { type Middleware, type SWRConfiguration, unstable_serialize } from 'swr'
import { GameStatus } from '@Components/GameCard'
import { isRetryableHttpError } from '@Utils/HttpError'
import { useServerNow } from '@Utils/ServerClock'
import { OnceSWRConfig } from '@Hooks/useConfig'
import api, { ParticipationStatus } from '@Api'

export const GAME_TIMING_REFRESH_MS = 60_000

type LiveGameReadGeneration = {
  key: string
  generation: number
  validationObserved: boolean
  ready: boolean
}

/**
 * A persisted SWR value may paint immediately, but it is not authoritative
 * enough for redirects until this mounted game key completes a live read.
 */
const useLiveGameReadReady = (key: string, gameId: number | undefined, isValidating: boolean, error: unknown) => {
  const [state, setState] = useState<LiveGameReadGeneration>({
    key,
    generation: 0,
    validationObserved: false,
    ready: false,
  })
  const responseMatchesKey = gameId !== undefined && key === unstable_serialize(`/api/game/${gameId}`)

  useLayoutEffect(() => {
    setState((current) => {
      const active =
        current.key === key
          ? current
          : {
              key,
              generation: current.generation + 1,
              validationObserved: false,
              ready: false,
            }
      const validationObserved = active.validationObserved || isValidating
      const ready =
        active.ready ||
        (validationObserved && !isValidating && error === undefined && key.length > 0 && responseMatchesKey)

      if (active === current && validationObserved === current.validationObserved && ready === current.ready)
        return current
      return { ...active, validationObserved, ready }
    })
  }, [error, isValidating, key, responseMatchesKey])

  return state.key === key && state.ready && responseMatchesKey
}

export const shouldRetryGameTimingError = (error: unknown) => isRetryableHttpError(error)

/** Keep an already-rendered landing page through retryable timing read failures. */
export const shouldRedirectGameLandingError = (error: unknown, hasLoadedGame: boolean) =>
  error !== undefined && error !== null && (!hasLoadedGame || !shouldRetryGameTimingError(error))

export const gameTimingSWRConfig: SWRConfiguration = {
  ...OnceSWRConfig,
  refreshInterval: GAME_TIMING_REFRESH_MS,
  refreshWhenHidden: false,
  refreshWhenOffline: false,
  revalidateOnFocus: true,
  revalidateOnReconnect: true,
  shouldRetryOnError: shouldRetryGameTimingError,
}

/** Own one timing poll leader and one replaceable recovery timer per SWR key. */
export const createGameTimingSWRConfig = () => {
  type LeadershipListener = (isLeader: boolean) => void
  type DeferredRetry = { isActive: () => boolean; run: () => void }

  const subscribers = new Map<string, Map<symbol, LeadershipListener>>()
  const retryTimers = new Map<string, ReturnType<typeof setTimeout>>()
  const deferredRetries = new Map<string, DeferredRetry>()
  let removeActivityListeners: (() => void) | null = null
  const stopListeningForActivity = () => {
    removeActivityListeners?.()
    removeActivityListeners = null
  }
  const resumeDeferredRetries = () => {
    deferredRetries.forEach((retry, key) => {
      if (!subscribers.has(key)) {
        deferredRetries.delete(key)
      } else if (retry.isActive()) {
        deferredRetries.delete(key)
        retry.run()
      }
    })
    if (deferredRetries.size === 0) stopListeningForActivity()
  }
  const listenForActivity = () => {
    if (removeActivityListeners) return
    const currentDocument = typeof document === 'undefined' ? null : document
    const currentWindow = typeof window === 'undefined' ? null : window
    if (!currentDocument && !currentWindow) return
    currentDocument?.addEventListener('visibilitychange', resumeDeferredRetries)
    currentWindow?.addEventListener('focus', resumeDeferredRetries)
    currentWindow?.addEventListener('online', resumeDeferredRetries)
    removeActivityListeners = () => {
      currentDocument?.removeEventListener('visibilitychange', resumeDeferredRetries)
      currentWindow?.removeEventListener('focus', resumeDeferredRetries)
      currentWindow?.removeEventListener('online', resumeDeferredRetries)
    }
  }
  const cancel = (key: string) => {
    const timer = retryTimers.get(key)
    if (timer !== undefined) clearTimeout(timer)
    retryTimers.delete(key)
    deferredRetries.delete(key)
    if (deferredRetries.size === 0) stopListeningForActivity()
  }
  const subscribe = (key: string, listener: LeadershipListener) => {
    const token = Symbol(key)
    const members = subscribers.get(key) ?? new Map<symbol, LeadershipListener>()
    const isFirst = members.size === 0
    members.set(token, listener)
    subscribers.set(key, members)
    if (isFirst) listener(true)

    return () => {
      const current = subscribers.get(key)
      if (!current) return
      const wasLeader = current.keys().next().value === token
      current.delete(token)
      if (current.size === 0) {
        subscribers.delete(key)
        cancel(key)
      } else if (wasLeader) {
        current.values().next().value?.(true)
      }
    }
  }
  const cancelAll = () => {
    retryTimers.forEach((timer) => clearTimeout(timer))
    retryTimers.clear()
    deferredRetries.clear()
    stopListeningForActivity()
  }
  const scopeMiddleware: Middleware = (useSWRNext) =>
    function useSharedGameTimingPoll(key, fetcher, swrConfig) {
      const serializedKey = unstable_serialize(key)
      const [leaderKey, setLeaderKey] = useState<string | null>(null)

      useLayoutEffect(() => {
        if (!serializedKey) {
          setLeaderKey(null)
          return
        }
        return subscribe(serializedKey, (isLeader) => {
          setLeaderKey((current) => {
            const next = isLeader ? serializedKey : null
            return current === next ? current : next
          })
        })
      }, [serializedKey])

      return useSWRNext(key, fetcher, {
        ...swrConfig,
        refreshInterval: leaderKey === serializedKey ? GAME_TIMING_REFRESH_MS : 0,
      })
    }
  const config: SWRConfiguration = {
    ...gameTimingSWRConfig,
    use: [...(gameTimingSWRConfig.use ?? []), scopeMiddleware],
    onError: (_error, key) => cancel(key),
    onSuccess: (_data, key) => cancel(key),
    onDiscarded: cancel,
    onErrorRetry: (_error, key, _config, revalidate, options) => {
      if (!subscribers.has(key)) return
      cancel(key)
      retryTimers.set(
        key,
        setTimeout(() => {
          retryTimers.delete(key)
          if (!subscribers.has(key)) return
          const visible = _config.refreshWhenHidden || _config.isVisible()
          const online = _config.refreshWhenOffline || _config.isOnline()
          if (!visible || !online) {
            // Keep no dormant timer chain. One shared listener resumes this
            // deduplicating revalidator once visibility and connectivity agree.
            deferredRetries.set(key, {
              isActive: () =>
                (_config.refreshWhenHidden || _config.isVisible()) &&
                (_config.refreshWhenOffline || _config.isOnline()),
              run: () => void revalidate(options),
            })
            listenForActivity()
            return
          }
          void revalidate(options)
        }, GAME_TIMING_REFRESH_MS)
      )
    },
  }
  return { config, cancelAll, subscribe }
}

const sharedGameTimingOwner = createGameTimingSWRConfig()

export const useGameTimingSWRConfig = () => sharedGameTimingOwner.config

/**
 * Publish one authoritative final snapshot when a lifecycle-owned poller stops.
 * A push feed can provide its shutdown fence so the snapshot cannot race ahead
 * of listener removal and miss a commit whose boundary broadcast is discarded.
 */
export const useRevalidateWhenPollingStops = (
  polling: boolean,
  revalidate: () => unknown,
  waitForStop?: () => Promise<unknown>
) => {
  const wasPolling = useRef(polling)

  useEffect(() => {
    const stopped = wasPolling.current && !polling
    wasPolling.current = polling
    if (!stopped) return

    void Promise.resolve()
      .then(() => waitForStop?.())
      .catch(() => undefined)
      .then(revalidate)
  }, [polling, revalidate, waitForStop])
}

export const useRecentGames = () => {
  const timingConfig = useGameTimingSWRConfig()
  const { data, mutate, error } = api.game.useGameRecentGames({ limit: 7 }, timingConfig)

  // Guard against SWR hydrating a stale non-array value from persistent
  // cache (e.g. an old 302/HTML response from a misconfigured proxy).
  return { recentGames: Array.isArray(data) ? data : undefined, error, mutate }
}

export const getGameStatus = (game?: { start?: number; end?: number }, now: Dayjs = dayjs()) => {
  const startTime = dayjs(game?.start)
  const endTime = dayjs(game?.end)

  const hasFiniteWindow =
    typeof game?.start === 'number' &&
    Number.isFinite(game.start) &&
    typeof game.end === 'number' &&
    Number.isFinite(game.end) &&
    startTime.isValid() &&
    endTime.isValid()
  const totalMilliseconds = hasFiniteWindow ? endTime.diff(startTime) : 0
  const validWindow = totalMilliseconds > 0
  const total = validWindow ? totalMilliseconds / 60_000 : 0
  const started = validWindow && !now.isBefore(startTime)
  const finished = validWindow && !now.isBefore(endTime)
  const elapsed = validWindow ? now.diff(startTime) : 0
  const progress = started ? (finished ? 1 : Math.min(1, Math.max(0, elapsed / totalMilliseconds))) : 0
  const status = started ? (finished ? GameStatus.Ended : GameStatus.OnGoing) : GameStatus.Coming

  return {
    startTime,
    endTime,
    finished,
    started,
    progress: Number.isFinite(progress) ? progress * 100 : 0,
    total,
    status,
  }
}

/** Reactive lifecycle projection driven by the shared, server-corrected clock. */
export const useGameStatus = (game?: { start?: number; end?: number }) => {
  const now = useServerNow()
  return { ...getGameStatus(game, now), now }
}

/** Duration shown beside a lifecycle status, using the same corrected clock. */
export const getGameDurationMinutes = (status: GameStatus, startTime: Dayjs, endTime: Dayjs, now: Dayjs) =>
  Math.max(0, status === GameStatus.OnGoing ? endTime.diff(now, 'minute') : endTime.diff(startTime, 'minute'))

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
  const timingConfig = useGameTimingSWRConfig()
  const { data: game, error, isValidating, mutate } = api.game.useGameGame(numId, timingConfig, numId > 0)

  return { game, error, isValidating, mutate, status: game?.status ?? ParticipationStatus.Unsubmitted }
}

/** Game data plus a per-route live-read gate for lifecycle/access redirects. */
export const useGameAccess = (numId: number) => {
  const gameState = useGame(numId)
  const gameKey = unstable_serialize(numId > 0 ? `/api/game/${numId}` : null)
  const liveReadReady = useLiveGameReadReady(gameKey, gameState.game?.id, gameState.isValidating, gameState.error)

  return { ...gameState, liveReadReady }
}

export const useGameScoreboard = (numId: number, isTabActive: boolean = true) => {
  const { game } = useGame(numId)
  const { status } = useGameStatus(game)
  const polling = status === GameStatus.OnGoing && isTabActive

  const {
    data: scoreboard,
    error,
    mutate,
  } = api.game.useGameScoreboard(numId, {
    ...OnceSWRConfig,
    refreshInterval: polling ? 30 * 1000 : 0,
  })
  useRevalidateWhenPollingStops(polling, mutate)

  return { scoreboard, error, mutate }
}

export const useGameTeamInfo = (numId: number, shouldPoll: boolean = true) => {
  const { game } = useGame(numId)
  const { status } = useGameStatus(game)
  const polling = status === GameStatus.OnGoing && shouldPoll

  const {
    data: teamInfo,
    error,
    mutate,
  } = api.game.useGameChallengesWithTeamInfo(numId, {
    ...OnceSWRConfig,
    shouldRetryOnError: false,
    refreshInterval: polling ? 10 * 1000 : 0,
  })
  useRevalidateWhenPollingStops(polling, mutate)

  return { teamInfo, game, error, mutate }
}

/** A&D — player state poll (own team's containers + flags). Pass doFetch=false
 *  to skip the request entirely (e.g. on pages that only conditionally need it). */
export const useAdState = (numId: number, doFetch: boolean = true) => {
  const { game } = useGame(numId)
  const { status } = useGameStatus(game)
  const polling = doFetch && status === GameStatus.OnGoing
  const {
    data: adState,
    error,
    mutate,
  } = api.game.useGameAdState(
    numId,
    {
      ...OnceSWRConfig,
      shouldRetryOnError: false,
      refreshInterval: polling ? 10 * 1000 : 0,
    },
    doFetch
  )
  useRevalidateWhenPollingStops(polling, mutate)
  return { adState, error, mutate }
}

/** Official A&D epoch scoreboard poll. */
export const useAdScoreboard = (numId: number, doFetch: boolean = true) => {
  const { game } = useGame(numId)
  const { status } = useGameStatus(game)
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
  /** Marker is exclusive boot2root control; Api is concurrent Leaderboard scoring. */
  claimSource: 'Api' | 'Marker' | string
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
  /** Marker acquisition, or Leaderboard verified activity. */
  acquisitionRate: number
  /** Marker control, or Leaderboard normalized objective performance. */
  controlRate: number
  /** Marker reliability, or Leaderboard share of finalized waves with the Crown. */
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
  /** Weighted point numerator behind the finalized event average. */
  settledEpochPoints: number
  /** Finalized epoch weight behind the finalized event average. */
  settledEpochWeight: number
  /** Weighted point numerator including the open epoch projection. */
  projectedEpochPoints: number
  /** Finalized plus open epoch weight behind the projection. */
  projectedEpochWeight: number
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
  const { status } = useGameStatus(game)
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

export interface CombinedMode {
  active: boolean
  /** Locked number of enabled, approved challenges in this format. */
  challengeCount: number
  /** Constant challenge-count share in [0, 1]. */
  weight: number
}

export interface CombinedScoreComponent {
  active: boolean
  score: number
  projectedScore: number
  earnedPoints?: number
  attainablePoints?: number
}

export interface CombinedScoreboardItem {
  id: number
  name: string
  avatar: string | null
  divisionId: number | null
  division: string | null
  rank: number
  divisionRank: number | null
  score: number
  projectedScore: number
  components: {
    jeopardy: CombinedScoreComponent
    attackDefense: CombinedScoreComponent
    koth: CombinedScoreComponent
  }
}

export interface CombinedScoreboardModel {
  /** Unix milliseconds. */
  generatedAt: number
  /** Unix milliseconds. */
  freeze: number | null
  isFrozenView: boolean
  fullySettled: boolean
  modes: {
    jeopardy: CombinedMode
    attackDefense: CombinedMode
    koth: CombinedMode
  }
  divisions: { id: number; name: string }[]
  items: CombinedScoreboardItem[]
}

/** Fixed, challenge-count-weighted 0-100 board across every active competition format. */
export const useCombinedScoreboard = (numId: number, doFetch: boolean = true) => {
  const { game } = useGame(numId)
  const { status } = useGameStatus(game)
  const {
    data: combinedScoreboard,
    error,
    mutate,
  } = useSWR<CombinedScoreboardModel>(doFetch && numId > 0 ? `/api/game/${numId}/scoreboard/combined` : null, {
    ...OnceSWRConfig,
    compare: Object.is,
    refreshInterval: (latest) => {
      if (!doFetch) return 0
      return status === GameStatus.OnGoing || latest?.fullySettled !== true ? 10 * 1000 : 60 * 1000
    },
  })
  return { combinedScoreboard, error, mutate }
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
  const { status } = useGameStatus(game)
  const polling = status === GameStatus.OnGoing
  const {
    data: adminAdState,
    error,
    mutate,
  } = api.edit.useEditAdState(numId, {
    ...OnceSWRConfig,
    refreshInterval: polling ? 5 * 1000 : 0,
  })
  useRevalidateWhenPollingStops(polling, mutate)
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
  claimSource: 'Api' | 'Marker' | string
  apiObserverConfigured: boolean
  apiObserverSecretHint: string | null
  /** Unix milliseconds. */
  apiLastObservationAt: number | null
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

export interface AdminKothObserverModel {
  challengeId: number
  claimSource: 'Api' | 'Marker' | string
  configured: boolean
  secretHint: string | null
  /** Frozen by the first accepted signed Leaderboard snapshot. */
  objectiveCount: number | null
  /** Stable ordered objective identities frozen with objectiveCount. */
  objectiveIds: string[] | null
  /** SHA-256 of the ordered objective schema. */
  objectiveSchemaHash: string | null
  /** Unix milliseconds. */
  createdAt: number | null
  /** Unix milliseconds. */
  rotatedAt: number | null
  /** Unix milliseconds. */
  lastUsedAt: number | null
  /** Unix milliseconds. */
  lastObservationAt: number | null
  contextPath: string
  observationPath: string
  /** Returned exactly once by credential creation/rotation. */
  secret?: string
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
  const { status } = useGameStatus(game)
  const polling = status === GameStatus.OnGoing
  const {
    data: adminKothState,
    error,
    mutate,
  } = useSWR<AdminKothStateModel>(numId > 0 ? `/api/edit/games/${numId}/ad/koth/state` : null, {
    ...OnceSWRConfig,
    shouldRetryOnError: false,
    refreshInterval: polling ? 5 * 1000 : 0,
  })
  useRevalidateWhenPollingStops(polling, mutate)
  return { adminKothState, error, mutate }
}
