import { httpErrorStatus, isRetryableHttpError } from '@Utils/HttpError'
import { getServerNowMilliseconds } from '@Utils/ServerClock'

type Timer = ReturnType<typeof setTimeout>
type DeferredRetry = {
  action: () => void
  generation: number
  isActive: () => boolean
}

export const MAX_PROFILE_RETRIES = 5
const MAX_BACKOFF_MS = 30_000
const MAX_RETRY_AFTER_MS = 5 * 60_000
export const PROFILE_RECOVERY_PROBE_MS = 5 * 60_000
const MAX_TIMER_DELAY_MS = 2_147_000_000

type ErrorWithResponseHeaders = {
  response?: {
    headers?: unknown
  }
}

export type ProfileErrorDisposition = 'anonymous' | 'banned' | 'retry' | 'stop'

export { httpErrorStatus } from '@Utils/HttpError'

export const profileErrorDisposition = (error: unknown): ProfileErrorDisposition => {
  if (error === null || error === undefined) return 'stop'
  const status = httpErrorStatus(error)
  if (status === 401) return 'anonymous'
  if (status === 403) return 'banned'
  if (isRetryableHttpError(error)) return 'retry'
  return 'stop'
}

const responseHeader = (error: unknown, name: string): string | null => {
  if (!error || typeof error !== 'object') return null
  const headers = (error as ErrorWithResponseHeaders).response?.headers
  if (!headers || typeof headers !== 'object') return null

  const getter = (headers as { get?: unknown }).get
  if (typeof getter === 'function') {
    const value = getter.call(headers, name)
    return typeof value === 'string' ? value : null
  }

  const record = headers as Record<string, unknown>
  const canonicalName = name.replace(
    /(^|-)([a-z])/g,
    (_match, separator: string, letter: string) => `${separator}${letter.toUpperCase()}`
  )
  const value = record[name] ?? record[canonicalName]
  if (typeof value === 'string') return value
  return typeof value === 'number' ? String(value) : null
}

export const retryAfterMilliseconds = (error: unknown, now: number = getServerNowMilliseconds()): number | null => {
  const header = responseHeader(error, 'retry-after')?.trim()
  if (!header) return null

  const seconds = Number(header)
  if (Number.isFinite(seconds) && seconds >= 0) return seconds * 1_000

  const date = Date.parse(header)
  if (!Number.isFinite(date)) return null
  const responseDate = Date.parse(responseHeader(error, 'date') ?? '')
  const referenceTime = Number.isFinite(responseDate) ? responseDate : now
  return Math.max(0, date - referenceTime)
}

/** Returns null when an automatic retry would wait too long to remain useful. */
export const profileRetryDelay = (
  error: unknown,
  retryCount: number,
  random: () => number = Math.random,
  now: number = getServerNowMilliseconds()
): number | null => {
  if (retryCount >= MAX_PROFILE_RETRIES || profileErrorDisposition(error) !== 'retry') return null

  const exponent = Math.min(Math.max(0, retryCount), 5)
  const jitter = 0.8 + Math.min(1, Math.max(0, random())) * 0.4
  const backoff = Math.min(MAX_BACKOFF_MS, Math.round(1_000 * 2 ** exponent * jitter))
  const retryAfter = retryAfterMilliseconds(error, now)
  if (retryAfter !== null && retryAfter > MAX_RETRY_AFTER_MS) return null
  return Math.max(backoff, retryAfter ?? 0)
}

/** Keep one low-frequency recovery probe after the fast retry budget is exhausted. */
export const profileRecoveryProbeDelay = (error: unknown, now: number = getServerNowMilliseconds()): number | null => {
  if (profileErrorDisposition(error) !== 'retry') return null
  const retryAfter = retryAfterMilliseconds(error, now) ?? 0
  return Math.min(MAX_TIMER_DELAY_MS, Math.max(PROFILE_RECOVERY_PROBE_MS, retryAfter))
}

export const profileRetryScheduleDelay = (
  error: unknown,
  retryCount: number,
  random: () => number = Math.random,
  now: number = getServerNowMilliseconds()
) => profileRetryDelay(error, retryCount, random, now) ?? profileRecoveryProbeDelay(error, now)

/** Owns the sole pending retry so later errors and successful reads supersede it. */
export const createProfileRetryTimers = () => {
  let timer: Timer | null = null
  let deferredRetry: DeferredRetry | null = null
  let generation = 0
  let removeActivityListeners: (() => void) | null = null

  const stopListeningForActivity = () => {
    removeActivityListeners?.()
    removeActivityListeners = null
  }
  const resumeDeferredRetry = () => {
    const retry = deferredRetry
    if (!retry || generation !== retry.generation || !retry.isActive()) return
    deferredRetry = null
    stopListeningForActivity()
    retry.action()
  }
  const listenForActivity = () => {
    if (removeActivityListeners) return
    const currentDocument = typeof document === 'undefined' ? null : document
    const currentWindow = typeof window === 'undefined' ? null : window
    if (!currentDocument && !currentWindow) return
    currentDocument?.addEventListener('visibilitychange', resumeDeferredRetry)
    currentWindow?.addEventListener('focus', resumeDeferredRetry)
    currentWindow?.addEventListener('online', resumeDeferredRetry)
    removeActivityListeners = () => {
      currentDocument?.removeEventListener('visibilitychange', resumeDeferredRetry)
      currentWindow?.removeEventListener('focus', resumeDeferredRetry)
      currentWindow?.removeEventListener('online', resumeDeferredRetry)
    }
  }

  const clearPending = () => {
    if (timer !== null) clearTimeout(timer)
    timer = null
    deferredRetry = null
    stopListeningForActivity()
  }

  return {
    schedule(delay: number, action: () => void, isActive: () => boolean = () => true) {
      generation += 1
      const scheduledGeneration = generation
      clearPending()
      timer = setTimeout(() => {
        timer = null
        if (generation !== scheduledGeneration) return
        if (!isActive()) {
          deferredRetry = { action, generation: scheduledGeneration, isActive }
          listenForActivity()
          return
        }
        action()
      }, delay)
    },
    cancel() {
      generation += 1
      clearPending()
    },
    pending: () => (timer === null && deferredRetry === null ? 0 : 1),
  }
}
