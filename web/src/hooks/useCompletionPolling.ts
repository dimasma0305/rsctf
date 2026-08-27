import { useEffect, useRef, useState } from 'react'
import type { KeyedMutator, SWRConfiguration } from 'swr'
import { isRetryableHttpError } from '@Utils/HttpError'
import { retryAfterMilliseconds } from '@Utils/ProfileRetry'
import { OnceSWRConfig } from '@Hooks/useConfig'

export const MAX_POLL_ERROR_RETRIES = 5
export const MAX_SCOREBOARD_WARMUP_POLLS = 6
export const MAX_SCOREBOARD_SETTLEMENT_POLLS = 12

const MAX_ERROR_BACKOFF_MS = 30_000
const MAX_RETRY_AFTER_MS = 5 * 60_000
const MAX_SETTLEMENT_INTERVAL_MS = 60_000

export type ScoreboardPollLifecycle = 'coming' | 'ongoing' | 'ended'

/**
 * SWR owns the initial read and cache only. A mounted completion poller owns
 * every later request, so interval and error-retry schedules cannot overlap.
 */
export const CompletionPollSWRConfig: SWRConfiguration = {
  ...OnceSWRConfig,
  refreshInterval: 0,
  refreshWhenHidden: false,
  refreshWhenOffline: false,
  revalidateOnFocus: false,
  revalidateOnReconnect: false,
  shouldRetryOnError: false,
}

const boundedRandom = (random: () => number) => Math.min(1, Math.max(0, random()))

export const jitterPollingDelay = (delay: number, random: () => number = Math.random) =>
  Math.max(1, Math.round(delay * (0.9 + boundedRandom(random) * 0.2)))

export const pollErrorIsTransient = (error: unknown) => isRetryableHttpError(error)

/**
 * Return one bounded retry delay after a completed failure. `completedFailures`
 * includes the initial failed read, so five automatic retries produce at most
 * six requests during one uninterrupted outage.
 */
export const pollErrorRetryDelay = (
  error: unknown,
  completedFailures: number,
  random: () => number = Math.random,
  now?: number
): number | null => {
  if (!pollErrorIsTransient(error) || completedFailures < 1 || completedFailures > MAX_POLL_ERROR_RETRIES) {
    return null
  }

  const exponent = Math.min(completedFailures - 1, 5)
  const jitter = 0.8 + boundedRandom(random) * 0.4
  const backoff = Math.min(MAX_ERROR_BACKOFF_MS, Math.round(1_000 * 2 ** exponent * jitter))
  const retryAfter = retryAfterMilliseconds(error, now)

  // Never retry earlier than the server requested. A very long Retry-After is
  // terminal for this mounted page rather than a retained browser timer.
  if (retryAfter !== null && retryAfter > MAX_RETRY_AFTER_MS) return null
  return Math.max(backoff, retryAfter ?? 0)
}

/** Explicit, bounded warmup/final-settlement policy for live engine boards. */
export const eventScoreboardPollDelay = (
  lifecycle: ScoreboardPollLifecycle,
  fullySettled: boolean | undefined,
  completedSuccesses: number,
  liveIntervalMs: number,
  random: () => number = Math.random
): number | null => {
  if (lifecycle === 'ongoing') return jitterPollingDelay(liveIntervalMs, random)

  if (lifecycle === 'coming') {
    return completedSuccesses < MAX_SCOREBOARD_WARMUP_POLLS ? jitterPollingDelay(liveIntervalMs, random) : null
  }

  if (fullySettled === true || completedSuccesses >= MAX_SCOREBOARD_SETTLEMENT_POLLS) return null
  const settlementDelay = Math.min(
    MAX_SETTLEMENT_INTERVAL_MS,
    liveIntervalMs * 2 ** Math.min(Math.max(0, completedSuccesses - 1), 3)
  )
  return jitterPollingDelay(settlementDelay, random)
}

const pageCanPoll = () => {
  const visible = typeof document === 'undefined' || document.visibilityState !== 'hidden'
  const online = typeof navigator === 'undefined' || navigator.onLine !== false
  return visible && online
}

/** Shared browser activity policy: no timer survives a hidden/offline interval. */
const usePollingPageActive = (enabled: boolean) => {
  const [active, setActive] = useState(() => enabled && pageCanPoll())

  useEffect(() => {
    if (!enabled) {
      setActive(false)
      return
    }

    const currentDocument = typeof document === 'undefined' ? null : document
    const currentWindow = typeof window === 'undefined' ? null : window
    const update = () => setActive(pageCanPoll())
    update()
    currentDocument?.addEventListener('visibilitychange', update)
    currentWindow?.addEventListener('online', update)
    currentWindow?.addEventListener('offline', update)
    return () => {
      currentDocument?.removeEventListener('visibilitychange', update)
      currentWindow?.removeEventListener('online', update)
      currentWindow?.removeEventListener('offline', update)
    }
  }, [enabled])

  return enabled && active
}

interface CompletionPollingOptions<T> {
  /** Stable SWR key. Use an empty string while the read is disabled. */
  key: string
  /** A phase change performs one immediate read, then starts the new cadence. */
  phase: string
  enabled: boolean
  data: T | undefined
  error: unknown
  isValidating: boolean
  mutate: KeyedMutator<T>
  successDelay: (data: T, completedSuccesses: number) => number | null
  random?: () => number
}

type PollOwner = {
  key: string
  phase: string
  wasValidating: boolean
  outcomeObserved: boolean
  completedSuccesses: number
  completedFailures: number
  immediate: boolean
}

const newOwner = (key: string, phase: string, isValidating: boolean, immediate: boolean): PollOwner => ({
  key,
  phase,
  wasValidating: isValidating,
  // A phase carries the previous snapshot. Do not count that stale value as a
  // successful read in the new warmup/settlement budget.
  outcomeObserved: immediate,
  completedSuccesses: 0,
  completedFailures: 0,
  immediate,
})

/**
 * Own exactly one completion-scheduled timer for one mounted SWR key. A request
 * must finish before its successor is scheduled, so slow responses cannot
 * accumulate in-flight work.
 */
export const useCompletionPolling = <T>({
  key,
  phase,
  enabled,
  data,
  error,
  isValidating,
  mutate,
  successDelay,
  random = Math.random,
}: CompletionPollingOptions<T>) => {
  const pageActive = usePollingPageActive(enabled)
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const owner = useRef<PollOwner>(newOwner(key, phase, isValidating, false))
  const successDelayRef = useRef(successDelay)
  const randomRef = useRef(random)
  successDelayRef.current = successDelay
  randomRef.current = random

  useEffect(() => {
    if (timer.current !== null) {
      clearTimeout(timer.current)
      timer.current = null
    }

    const previous = owner.current
    const keyChanged = previous.key !== key
    const phaseChanged = !keyChanged && previous.phase !== phase
    if (keyChanged || phaseChanged) {
      owner.current = newOwner(key, phase, isValidating, phaseChanged && enabled && key.length > 0)
    }
    const current = owner.current

    if (!enabled || key.length === 0) return

    const completed =
      (current.wasValidating && !isValidating) ||
      (!current.outcomeObserved && !isValidating && (data !== undefined || error !== undefined))
    current.wasValidating = isValidating
    if (completed) {
      current.outcomeObserved = true
      if (error !== undefined) {
        current.completedFailures += 1
      } else if (data !== undefined) {
        current.completedFailures = 0
        current.completedSuccesses += 1
      }
    }

    if (isValidating || !pageActive) return

    let delay: number | null = null
    if (current.immediate) {
      // A same-key phase change supersedes the previous phase's snapshot or
      // terminal error. Read once immediately; the completed result then owns
      // the bounded success/error cadence for the new phase.
      delay = 0
    } else if (error !== undefined) {
      delay = pollErrorRetryDelay(error, current.completedFailures, randomRef.current)
    } else if (data !== undefined) {
      delay = successDelayRef.current(data, current.completedSuccesses)
    }
    if (delay === null) return

    timer.current = setTimeout(() => {
      timer.current = null
      current.immediate = false
      if (!pageCanPoll()) return
      void mutate().catch(() => undefined)
    }, delay)

    return () => {
      if (timer.current !== null) {
        clearTimeout(timer.current)
        timer.current = null
      }
    }
  }, [data, enabled, error, isValidating, key, mutate, pageActive, phase])

  useEffect(
    () => () => {
      if (timer.current !== null) clearTimeout(timer.current)
      timer.current = null
    },
    []
  )
}
