import { httpErrorStatus, retryAfterMilliseconds } from '@Utils/ProfileRetry'
import { getServerNowMilliseconds } from '@Utils/ServerClock'
import { createUuid } from '@Utils/Uuid'

export const MAX_CHALLENGE_POLL_RETRIES = 3
export const MAX_CHALLENGE_RETRY_AFTER_MS = 5 * 60_000
const MAX_CHALLENGE_BACKOFF_MS = 30_000

type ErrorWithCode = {
  code?: unknown
  name?: unknown
}

export type ChallengeReadResource = 'challenge' | 'solvers'

export interface ChallengeReadFailure {
  resource: ChallengeReadResource
  /** Random, bounded identifier also recorded in the server request span. */
  requestId: string
  /** Optional upstream/server trace identifier from a response header. */
  serverTraceId?: string
  retryAfterMilliseconds?: number
}

const READ_FAILURES = new WeakMap<object, Map<ChallengeReadResource, ChallengeReadFailure>>()
const SAFE_DIAGNOSTIC_ID = /^[A-Za-z0-9._:-]{8,128}$/

type ErrorWithResponseHeaders = {
  response?: {
    headers?: unknown
  }
}

const responseHeader = (value: unknown, name: string): string | null => {
  if (!value || typeof value !== 'object') return null
  const headers = (value as ErrorWithResponseHeaders).response?.headers
  if (!headers || typeof headers !== 'object') return null
  const getter = (headers as { get?: unknown }).get
  if (typeof getter === 'function') {
    const result = getter.call(headers, name)
    return typeof result === 'string' ? result : null
  }
  const record = headers as Record<string, unknown>
  const result = Object.entries(record).find(([key]) => key.toLowerCase() === name.toLowerCase())?.[1]
  return typeof result === 'string' ? result : typeof result === 'number' ? String(result) : null
}

const safeDiagnosticId = (value: string | null) => {
  const normalized = value?.trim() ?? ''
  return SAFE_DIAGNOSTIC_ID.test(normalized) ? normalized : undefined
}

const responseTraceId = (error: unknown) => {
  for (const name of ['x-rsctf-request-id', 'x-request-id', 'x-correlation-id']) {
    const identifier = safeDiagnosticId(responseHeader(error, name))
    if (identifier) return identifier
  }
  const traceparent = responseHeader(error, 'traceparent')?.trim()
  const traceId = traceparent?.match(/^\d\d-([0-9a-f]{32})-[0-9a-f]{16}-[0-9a-f]{2}$/i)?.[1]
  return safeDiagnosticId(traceId ?? null)
}

const challengeRetryAfterMilliseconds = (error: unknown, now: number): number | null => {
  const headerDelay = retryAfterMilliseconds(error, now)
  const retryAt = error && typeof error === 'object' ? (error as { retryAt?: unknown }).retryAt : undefined
  const deadlineDelay = typeof retryAt === 'number' && Number.isFinite(retryAt) ? Math.max(0, retryAt - now) : null
  if (headerDelay === null) return deadlineDelay
  if (deadlineDelay === null) return headerDelay
  return Math.max(headerDelay, deadlineDelay)
}

export const createChallengeRequestId = (resource: ChallengeReadResource) => `challenge-${resource}-${createUuid()}`

export const challengeRequestHeaders = (requestId: string) => ({
  'x-rsctf-request-id': requestId,
})

/**
 * Retain the original transport error (notably EventVpnAccessError and AxiosError)
 * while attaching a safe support reference outside the serialized/logged payload.
 */
export const captureChallengeReadFailure = (error: unknown, resource: ChallengeReadResource, requestId: string) => {
  const normalized =
    error && typeof error === 'object' ? error : new Error('Challenge request failed', { cause: error })
  const retryAfter = challengeRetryAfterMilliseconds(normalized, getServerNowMilliseconds())
  const failures = READ_FAILURES.get(normalized) ?? new Map<ChallengeReadResource, ChallengeReadFailure>()
  failures.set(resource, {
    resource,
    requestId,
    serverTraceId: responseTraceId(normalized),
    retryAfterMilliseconds:
      retryAfter !== null && retryAfter >= 0 && retryAfter <= MAX_CHALLENGE_RETRY_AFTER_MS ? retryAfter : undefined,
  })
  READ_FAILURES.set(normalized, failures)
  return normalized
}

export const challengeReadFailure = (error: unknown, resource: ChallengeReadResource) =>
  error && typeof error === 'object' ? READ_FAILURES.get(error)?.get(resource) : undefined

export class NonJsonResponseError extends Error {
  readonly status: number
  readonly response: { status: number; headers?: unknown }

  constructor(status: number, contentType: string | null, headers?: unknown) {
    super(`Expected a JSON response, received ${contentType || 'an unknown content type'}`)
    this.name = 'NonJsonResponseError'
    this.status = status
    this.response = { status, headers }
  }
}

export const isAbortError = (error: unknown) => {
  if (!error || typeof error !== 'object') return false
  const candidate = error as ErrorWithCode
  return candidate.name === 'AbortError' || candidate.name === 'CanceledError' || candidate.code === 'ERR_CANCELED'
}

export const isJsonContentType = (contentType: unknown) =>
  typeof contentType === 'string' && /^(application|text)\/(?:[\w.+-]*\+)?json(?:\s*;|$)/i.test(contentType.trim())

export const assertJsonResponse = <T>(response: {
  status: number
  data: T
  headers?: { get?: (name: string) => unknown } | Record<string, unknown>
}): T => {
  const headers = response.headers
  const getter = headers && typeof headers === 'object' ? (headers as { get?: unknown }).get : undefined
  const contentType =
    typeof getter === 'function'
      ? getter.call(headers, 'content-type')
      : (headers as Record<string, unknown> | undefined)?.['content-type']

  if (!isJsonContentType(contentType)) {
    throw new NonJsonResponseError(response.status, typeof contentType === 'string' ? contentType : null, headers)
  }
  return response.data
}

export const isChallengePollRetryable = (error: unknown) => {
  if (isAbortError(error) || error instanceof NonJsonResponseError) return false
  const status = httpErrorStatus(error)
  return status === null || status === 408 || status === 425 || status === 429 || status >= 500
}

/** A bounded retry delay. Permanent failures and excessive Retry-After values stop. */
export const challengePollRetryDelay = (
  error: unknown,
  retryCount: number,
  random: () => number = Math.random,
  now: number = getServerNowMilliseconds()
): number | null => {
  if (retryCount >= MAX_CHALLENGE_POLL_RETRIES || !isChallengePollRetryable(error)) return null

  const retryAfter = challengeRetryAfterMilliseconds(error, now)
  if (retryAfter !== null && retryAfter > MAX_CHALLENGE_RETRY_AFTER_MS) return null
  const jitter = 0.8 + Math.min(1, Math.max(0, random())) * 0.4
  const backoff = Math.min(MAX_CHALLENGE_BACKOFF_MS, Math.round(1_000 * 2 ** retryCount * jitter))
  return Math.max(backoff, retryAfter ?? 0)
}

export const createChallengePollOwner = () => {
  let controller: AbortController | null = null
  let retryTimer: ReturnType<typeof setTimeout> | null = null

  const cancelRetry = () => {
    if (retryTimer !== null) clearTimeout(retryTimer)
    retryTimer = null
  }

  return {
    begin() {
      controller?.abort()
      controller = new AbortController()
      return controller
    },
    finish(completed: AbortController) {
      if (controller === completed) controller = null
    },
    schedule(delay: number, action: () => void) {
      cancelRetry()
      retryTimer = setTimeout(() => {
        retryTimer = null
        action()
      }, delay)
    },
    cancel() {
      controller?.abort()
      controller = null
      cancelRetry()
    },
    pendingRetryCount() {
      return retryTimer === null ? 0 : 1
    },
  }
}

type RecoveryEntry = {
  dueAt: number
  action: () => boolean | void
  deferred: boolean
}

/**
 * One timer for all reads owned by a challenge modal. Detail and solver failures
 * retain independent Retry-After deadlines, but simultaneous recovery never
 * creates a timer per resource.
 */
export const createChallengeRecoveryOwner = () => {
  const entries = new Map<string, RecoveryEntry>()
  let timer: ReturnType<typeof setTimeout> | null = null
  let removeActivityListeners: (() => void) | null = null

  const stopListeningForActivity = () => {
    removeActivityListeners?.()
    removeActivityListeners = null
  }

  const syncActivityListeners = () => {
    const hasDeferred = Array.from(entries.values()).some((entry) => entry.deferred)
    if (!hasDeferred) {
      stopListeningForActivity()
      return
    }
    if (removeActivityListeners) return
    const currentDocument = typeof document === 'undefined' ? null : document
    const currentWindow = typeof window === 'undefined' ? null : window
    if (!currentDocument && !currentWindow) return
    const resume = () => {
      for (const [key, entry] of entries) {
        if (!entry.deferred || entry.action() === false) continue
        entries.delete(key)
      }
      syncActivityListeners()
      arm()
    }
    currentDocument?.addEventListener('visibilitychange', resume)
    currentWindow?.addEventListener('focus', resume)
    currentWindow?.addEventListener('online', resume)
    removeActivityListeners = () => {
      currentDocument?.removeEventListener('visibilitychange', resume)
      currentWindow?.removeEventListener('focus', resume)
      currentWindow?.removeEventListener('online', resume)
    }
  }

  function arm() {
    if (timer !== null) clearTimeout(timer)
    timer = null
    syncActivityListeners()
    const scheduled = Array.from(entries.values()).filter((entry) => !entry.deferred)
    if (scheduled.length === 0) return
    const now = Date.now()
    const nextDue = Math.min(...scheduled.map((entry) => entry.dueAt))
    timer = setTimeout(
      () => {
        timer = null
        const readyAt = Date.now()
        for (const [key, entry] of entries) {
          if (entry.deferred || entry.dueAt > readyAt) continue
          if (entry.action() === false) entry.deferred = true
          else entries.delete(key)
        }
        syncActivityListeners()
        arm()
      },
      Math.max(0, nextDue - now)
    )
  }

  return {
    schedule(key: string, delay: number, action: () => boolean | void) {
      entries.set(key, { dueAt: Date.now() + Math.max(0, delay), action, deferred: false })
      arm()
    },
    cancel(key: string) {
      if (!entries.delete(key)) return
      syncActivityListeners()
      arm()
    },
    cancelAll() {
      entries.clear()
      if (timer !== null) clearTimeout(timer)
      timer = null
      stopListeningForActivity()
    },
    pendingEntryCount: () => entries.size,
    pendingTimerCount: () => (timer === null ? 0 : 1),
  }
}

export type ChallengeRecoveryOwner = ReturnType<typeof createChallengeRecoveryOwner>
