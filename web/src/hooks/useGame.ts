import dayjs, { Dayjs } from 'dayjs'
import { TFunction } from 'i18next'
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import useSWR, { type Middleware, type SWRConfiguration, unstable_serialize } from 'swr'
import { GameStatus } from '@Components/GameCard'
import { isRetryableHttpError } from '@Utils/HttpError'
import { useServerNow } from '@Utils/ServerClock'
import { useViewerIdentity, viewerScopedKey } from '@Utils/ViewerIdentity'
import {
  CompletionPollSWRConfig,
  eventScoreboardPollDelay,
  jitterPollingDelay,
  useCompletionPolling,
} from '@Hooks/useCompletionPolling'
import { OnceSWRConfig } from '@Hooks/useConfig'
import api, {
  type AdEngineMetadataModel,
  type AdGameStateModel,
  type AdLiveStateModel,
  ParticipationStatus,
} from '@Api'

export const GAME_TIMING_REFRESH_MS = 60_000
export const GAME_TIMING_RETRY_CAP_MS = 5 * 60_000
export const GAME_ACCESS_READ_READY_MS = 5_000
const MAX_RECENT_GAME_READS = 128

type GameAccessReadSnapshot = {
  id: number
  start: unknown
  end: unknown
  status: unknown
  practiceMode: unknown
  serverTime: unknown
}

type RecentGameRead = {
  expiresAt: number
  snapshot: GameAccessReadSnapshot
}

const monotonicMilliseconds = () => (typeof performance === 'undefined' ? Date.now() : performance.now())

const gameAccessReadSnapshot = (data: unknown): GameAccessReadSnapshot | null => {
  if (!data || typeof data !== 'object') return null
  const record = data as Record<string, unknown>
  const id = record.id
  if (typeof id !== 'number' || !Number.isSafeInteger(id) || id <= 0) return null
  return {
    id,
    start: record.start,
    end: record.end,
    status: record.status,
    practiceMode: record.practiceMode,
    serverTime: record.serverTime,
  }
}

const sameGameAccessRead = (left: GameAccessReadSnapshot, right: GameAccessReadSnapshot) =>
  left.id === right.id &&
  Object.is(left.start, right.start) &&
  Object.is(left.end, right.end) &&
  Object.is(left.status, right.status) &&
  Object.is(left.practiceMode, right.practiceMode) &&
  Object.is(left.serverTime, right.serverTime)

const createRecentGameReads = (nowMilliseconds: () => number) => {
  const reads = new Map<string, RecentGameRead>()
  const prune = (now: number) => {
    reads.forEach((entry, key) => {
      if (entry.expiresAt <= now) reads.delete(key)
    })
  }
  const invalidate = (key: string) => reads.delete(key)
  const remember = (key: string, data: unknown) => {
    const snapshot = gameAccessReadSnapshot(data)
    if (!snapshot) {
      invalidate(key)
      return
    }
    const now = nowMilliseconds()
    prune(now)
    reads.delete(key)
    reads.set(key, { expiresAt: now + GAME_ACCESS_READ_READY_MS, snapshot })
    while (reads.size > MAX_RECENT_GAME_READS) {
      const oldestKey = reads.keys().next().value
      if (oldestKey === undefined) break
      reads.delete(oldestKey)
    }
  }
  const matches = (key: string, data: unknown) => {
    const now = nowMilliseconds()
    const entry = reads.get(key)
    if (!entry) return false
    if (entry.expiresAt <= now) {
      reads.delete(key)
      return false
    }
    const snapshot = gameAccessReadSnapshot(data)
    return snapshot !== null && sameGameAccessRead(entry.snapshot, snapshot)
  }
  const clear = () => reads.clear()
  return { clear, invalidate, matches, remember }
}

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
const useLiveGameReadReady = (
  key: string,
  expectedGameId: number,
  responseGameId: number | undefined,
  isValidating: boolean,
  error: unknown,
  recentSuccessfulRead: boolean
) => {
  const [state, setState] = useState<LiveGameReadGeneration>({
    key,
    generation: 0,
    validationObserved: false,
    ready: recentSuccessfulRead,
  })
  const responseMatchesKey = expectedGameId > 0 && responseGameId === expectedGameId

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
        recentSuccessfulRead ||
        (validationObserved && !isValidating && error === undefined && key.length > 0 && responseMatchesKey)

      if (active === current && validationObserved === current.validationObserved && ready === current.ready)
        return current
      return { ...active, validationObserved, ready }
    })
  }, [error, isValidating, key, recentSuccessfulRead, responseMatchesKey])

  return responseMatchesKey && ((state.key === key && state.ready) || recentSuccessfulRead)
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

/** Equal jitter avoids immediate retries while spreading clients across each backoff window. */
export const gameTimingRetryDelay = (retryCount: number, random: () => number = Math.random) => {
  const normalizedRetryCount = Number.isFinite(retryCount) ? Math.max(1, Math.floor(retryCount)) : 1
  const cappedExponent = Math.min(
    normalizedRetryCount - 1,
    Math.ceil(Math.log2(GAME_TIMING_RETRY_CAP_MS / GAME_TIMING_REFRESH_MS))
  )
  const backoffCeiling = Math.min(GAME_TIMING_RETRY_CAP_MS, GAME_TIMING_REFRESH_MS * 2 ** cappedExponent)
  const jitter = Math.min(1, Math.max(0, random()))
  return Math.round(backoffCeiling / 2 + (backoffCeiling / 2) * jitter)
}

/** Own one timing poll leader and one replaceable recovery timer per SWR key. */
export const createGameTimingSWRConfig = (
  nowMilliseconds: () => number = monotonicMilliseconds,
  random: () => number = Math.random
) => {
  type LeadershipListener = (isLeader: boolean) => void
  type DeferredRetry = { isActive: () => boolean; run: () => void }

  const subscribers = new Map<string, Map<symbol, LeadershipListener>>()
  const retryTimers = new Map<string, ReturnType<typeof setTimeout>>()
  const deferredRetries = new Map<string, DeferredRetry>()
  const recentGameReads = createRecentGameReads(nowMilliseconds)
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
    recentGameReads.clear()
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
    onError: (_error, key) => {
      const serializedKey = unstable_serialize(key)
      cancel(serializedKey)
      recentGameReads.invalidate(serializedKey)
    },
    onSuccess: (data, key) => {
      const serializedKey = unstable_serialize(key)
      cancel(serializedKey)
      recentGameReads.remember(serializedKey, data)
    },
    onDiscarded: (key) => cancel(unstable_serialize(key)),
    onErrorRetry: (_error, key, _config, revalidate, options) => {
      const serializedKey = unstable_serialize(key)
      if (!subscribers.has(serializedKey)) return
      cancel(serializedKey)
      retryTimers.set(
        serializedKey,
        setTimeout(
          () => {
            retryTimers.delete(serializedKey)
            if (!subscribers.has(serializedKey)) return
            const visible = _config.refreshWhenHidden || _config.isVisible()
            const online = _config.refreshWhenOffline || _config.isOnline()
            if (!visible || !online) {
              // Keep no dormant timer chain. One shared listener resumes this
              // deduplicating revalidator once visibility and connectivity agree.
              deferredRetries.set(serializedKey, {
                isActive: () =>
                  (_config.refreshWhenHidden || _config.isVisible()) &&
                  (_config.refreshWhenOffline || _config.isOnline()),
                run: () => void revalidate(options),
              })
              listenForActivity()
              return
            }
            void revalidate(options)
          },
          gameTimingRetryDelay(options.retryCount, random)
        )
      )
    },
  }
  return {
    config,
    cancelAll,
    subscribe,
    hasRecentSuccessfulGameRead: (key: string, data: unknown) => recentGameReads.matches(key, data),
  }
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
    let cancelled = false
    const stopped = wasPolling.current && !polling
    wasPolling.current = polling
    if (!stopped) return

    void Promise.resolve()
      .then(() => waitForStop?.())
      .catch(() => undefined)
      .then(() => {
        if (!cancelled) return revalidate()
      })

    // Query-scoped callbacks change with their page, filter, type, or search.
    // Retire an older post-stop chain before it can publish into that new scope.
    return () => {
      cancelled = true
    }
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
  const { scope } = useViewerIdentity()
  const gameKey = unstable_serialize(viewerScopedKey(numId > 0 ? `/api/game/${numId}` : null, scope))
  const recentSuccessfulRead = sharedGameTimingOwner.hasRecentSuccessfulGameRead(gameKey, gameState.game)
  const liveReadReady = useLiveGameReadReady(
    gameKey,
    numId,
    gameState.game?.id,
    gameState.isValidating,
    gameState.error,
    recentSuccessfulRead
  )

  return { ...gameState, liveReadReady }
}

export const useGameScoreboardRead = (numId: number) => {
  const {
    data: scoreboard,
    error,
    isValidating,
    mutate,
  } = api.game.useGameScoreboard(
    numId,
    {
      ...CompletionPollSWRConfig,
      // Conditional 304 reads return the exact retained object. Object identity
      // avoids a recursive comparison over the full roster/challenge matrix.
      compare: Object.is,
    },
    numId > 0
  )

  return { scoreboard, error, isValidating, mutate }
}

export const useGameScoreboardPoll = (
  numId: number,
  status: GameStatus,
  isTabActive: boolean,
  { scoreboard, error, isValidating, mutate }: ReturnType<typeof useGameScoreboardRead>
) => {
  useCompletionPolling({
    key: numId > 0 ? `/api/game/${numId}/scoreboard` : '',
    // This read remains mounted for tab discovery. Include tab ownership in
    // the phase so returning to Jeopardy performs one fresh read immediately.
    phase: `${status}:${isTabActive ? 'active' : 'inactive'}`,
    enabled: numId > 0 && isTabActive,
    data: scoreboard,
    error,
    isValidating,
    mutate,
    // Standard Jeopardy has no asynchronous epoch settlement. A lifecycle
    // transition still causes one immediate final read before this returns null.
    successDelay: () => (status === GameStatus.OnGoing ? jitterPollingDelay(30_000) : null),
  })
}

export const useGameScoreboard = (numId: number, isTabActive: boolean = true) => {
  const { game } = useGame(numId)
  const { status } = useGameStatus(game)
  const query = useGameScoreboardRead(numId)
  useGameScoreboardPoll(numId, status, isTabActive, query)

  return query
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
    isValidating,
    mutate,
  } = api.game.useGameAdScoreboard(
    numId,
    {
      ...CompletionPollSWRConfig,
      // Every response has a new generatedAt version, so recursive comparison
      // only scans the full team/service matrix before reaching that difference.
      compare: Object.is,
    },
    doFetch && numId > 0
  )
  useCompletionPolling({
    key: doFetch && numId > 0 ? `/api/Game/${numId}/Ad/Scoreboard` : '',
    phase: status,
    enabled: doFetch && numId > 0,
    data: adScoreboard,
    error,
    isValidating,
    mutate,
    successDelay: (latest, completedSuccesses) =>
      eventScoreboardPollDelay(status, latest.fullySettled, completedSuccesses, 10_000),
  })
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
    isValidating,
    mutate,
  } = useSWR<KothScoreboardModel>(doFetch && numId > 0 ? `/api/game/${numId}/ad/koth/scoreboard` : null, {
    ...CompletionPollSWRConfig,
    // The conditional reader returns this exact object for a 304, preventing
    // the large KotH table from rendering an unchanged teams-by-hills matrix.
    compare: Object.is,
  })
  useCompletionPolling({
    key: doFetch && numId > 0 ? `/api/game/${numId}/ad/koth/scoreboard` : '',
    phase: status,
    enabled: doFetch && numId > 0,
    data: kothScoreboard,
    error,
    isValidating,
    mutate,
    successDelay: (latest, completedSuccesses) =>
      eventScoreboardPollDelay(status, latest.fullySettled, completedSuccesses, 10_000),
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
    isValidating,
    mutate,
  } = useSWR<CombinedScoreboardModel>(doFetch && numId > 0 ? `/api/game/${numId}/scoreboard/combined` : null, {
    ...CompletionPollSWRConfig,
    compare: Object.is,
  })
  useCompletionPolling({
    key: doFetch && numId > 0 ? `/api/game/${numId}/scoreboard/combined` : '',
    phase: status,
    enabled: doFetch && numId > 0,
    data: combinedScoreboard,
    error,
    isValidating,
    mutate,
    successDelay: (latest, completedSuccesses) =>
      eventScoreboardPollDelay(status, latest.fullySettled, completedSuccesses, 10_000),
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

export const ADMIN_OPERATOR_POLL_MS = 5_000
export const ADMIN_OPERATOR_GRID_POLL_MS = 30_000
export const ADMIN_OPERATOR_METADATA_POLL_MS = 60_000
export const ADMIN_OPERATOR_RETRY_LIMIT = 4

export type AdminOperatorView = 'ad' | 'koth'

export const adminOperatorView = (
  preferred: AdminOperatorView,
  metadata?: Pick<AdEngineMetadataModel, 'hasAttackDefense' | 'hasKoth'>
): AdminOperatorView => {
  if (!metadata) return preferred
  if (preferred === 'ad' && metadata.hasAttackDefense) return 'ad'
  if (preferred === 'koth' && metadata.hasKoth) return 'koth'
  return metadata.hasAttackDefense ? 'ad' : 'koth'
}

export const adminOperatorPolling = (
  metadata: Pick<AdEngineMetadataModel, 'start' | 'end'> | undefined,
  nowMilliseconds: number
) =>
  metadata !== undefined &&
  Number.isFinite(metadata.start) &&
  Number.isFinite(metadata.end) &&
  metadata.start <= nowMilliseconds &&
  nowMilliseconds < metadata.end

const operatorReadConfig = (refreshInterval: number): SWRConfiguration => ({
  ...OnceSWRConfig,
  refreshInterval,
  refreshWhenHidden: false,
  refreshWhenOffline: false,
  revalidateOnFocus: true,
  revalidateOnReconnect: true,
  shouldRetryOnError: isRetryableHttpError,
  errorRetryCount: ADMIN_OPERATOR_RETRY_LIMIT,
  errorRetryInterval: ADMIN_OPERATOR_POLL_MS,
})

/** One cheap authorized read decides which engine endpoint may be mounted. */
export const useAdminOperatorEngines = (numId: number) => {
  const { data, error, mutate } = useSWR<AdEngineMetadataModel>(
    numId > 0 ? `/api/edit/games/${numId}/ad/Engines` : null,
    operatorReadConfig(ADMIN_OPERATOR_METADATA_POLL_MS)
  )
  return { engineMetadata: data, error, mutate }
}

export const mergeAdminAdState = (
  snapshot: AdGameStateModel | undefined,
  live: AdLiveStateModel | undefined
): AdGameStateModel | undefined => {
  if (!snapshot || !live) return snapshot
  const liveByService = new Map(live.services.map((cell) => [cell.adTeamServiceId, cell]))
  return {
    ...snapshot,
    currentRound: live.currentRound,
    roundStartedAt: live.roundStartedAt,
    roundEndsAt: live.roundEndsAt,
    scoringPaused: live.scoringPaused,
    controlRevision: live.controlRevision,
    scoringPausedAt: live.scoringPausedAt,
    teams: snapshot.teams.map((team) => ({
      ...team,
      services: team.services.map((cell) => {
        const delta = liveByService.get(cell.adTeamServiceId)
        return delta
          ? {
              ...cell,
              lastCheckId: delta.lastCheckId,
              lastCheckStatus: delta.lastCheckStatus,
              currentFlag: delta.currentFlag,
            }
          : cell
      }),
    })),
  }
}

/** Refresh grid structure slowly while keeping verdict deltas on the live cadence. */
export const useAdminAdState = (numId: number, enabled: boolean, polling: boolean) => {
  const {
    data: grid,
    error: gridError,
    mutate: mutateGrid,
  } = api.edit.useEditAdState(
    numId,
    operatorReadConfig(polling ? ADMIN_OPERATOR_GRID_POLL_MS : 0),
    enabled && numId > 0
  )
  const {
    data: live,
    error: liveError,
    mutate: mutateLive,
  } = useSWR<AdLiveStateModel>(
    enabled && numId > 0 ? `/api/edit/games/${numId}/ad/Live` : null,
    operatorReadConfig(polling ? ADMIN_OPERATOR_POLL_MS : 0)
  )
  useRevalidateWhenPollingStops(enabled && polling, mutateGrid)
  useRevalidateWhenPollingStops(enabled && polling, mutateLive)
  const adminAdState = useMemo(() => mergeAdminAdState(grid, live), [grid, live])
  const mutate = async () => Promise.all([mutateGrid(), mutateLive()])
  return { adminAdState, error: gridError ?? liveError, mutate }
}

/** One KotH hill in the operator console — the shared container + its current king + verdict. */
export interface AdminKothHill extends KothLifecycleFields {
  challengeId: number
  title: string
  isEnabled: boolean
  controlRevision: number
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
  /** Unix milliseconds; identical for player/admin readers sharing this version. */
  scoringGeneratedAt: number
  latestRound: number
  /** Unix milliseconds. */
  currentRoundEndsAt: number | null
  scoringPaused: boolean
  controlRevision: number
  /** Unix milliseconds. */
  scoringPausedAt: number | null
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
  /** Monotonic credential-state revision used by rotate/revoke preconditions. */
  revision: number
  claimSource: 'Api' | 'Marker' | string
  configured: boolean
  /** The platform injects a lifecycle-bound signing credential into the active target. */
  managedTargetReporting: boolean
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
  /** Identifies a completed recoverable credential mutation result. */
  operationId?: string
  /** Returned only by the original authorized mutation/recovery operation. */
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
export const useAdminKothState = (numId: number, enabled: boolean, polling: boolean) => {
  const {
    data: adminKothState,
    error,
    mutate,
  } = useSWR<AdminKothStateModel>(
    enabled && numId > 0 ? `/api/edit/games/${numId}/ad/koth/state` : null,
    operatorReadConfig(polling ? ADMIN_OPERATOR_POLL_MS : 0)
  )
  useRevalidateWhenPollingStops(enabled && polling, mutate)
  return { adminKothState, error, mutate }
}
