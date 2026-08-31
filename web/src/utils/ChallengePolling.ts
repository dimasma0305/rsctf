import { httpErrorStatus, retryAfterMilliseconds } from '@Utils/ProfileRetry'
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

const READ_FAILURES = new WeakMap<object, ChallengeReadFailure>()
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
  const retryAfter = retryAfterMilliseconds(normalized)
  READ_FAILURES.set(normalized, {
    resource,
    requestId,
    serverTraceId: responseTraceId(normalized),
    retryAfterMilliseconds:
      retryAfter !== null && retryAfter >= 0 && retryAfter <= MAX_CHALLENGE_RETRY_AFTER_MS ? retryAfter : undefined,
  })
  return normalized
}

export const challengeReadFailure = (error: unknown) =>
  error && typeof error === 'object' ? READ_FAILURES.get(error) : undefined

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
  now: number = Date.now()
): number | null => {
  if (retryCount >= MAX_CHALLENGE_POLL_RETRIES || !isChallengePollRetryable(error)) return null

  const retryAfter = retryAfterMilliseconds(error, now)
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
  action: () => void
}

/**
 * One timer for all reads owned by a challenge modal. Detail and solver failures
 * retain independent Retry-After deadlines, but simultaneous recovery never
 * creates a timer per resource.
 */
export const createChallengeRecoveryOwner = () => {
  const entries = new Map<string, RecoveryEntry>()
  let timer: ReturnType<typeof setTimeout> | null = null

  const arm = () => {
    if (timer !== null) clearTimeout(timer)
    timer = null
    if (entries.size === 0) return
    const now = Date.now()
    const nextDue = Math.min(...Array.from(entries.values(), (entry) => entry.dueAt))
    timer = setTimeout(
      () => {
        timer = null
        const readyAt = Date.now()
        const ready: RecoveryEntry[] = []
        for (const [key, entry] of entries) {
          if (entry.dueAt > readyAt) continue
          entries.delete(key)
          ready.push(entry)
        }
        arm()
        for (const entry of ready) entry.action()
      },
      Math.max(0, nextDue - now)
    )
  }

  return {
    schedule(key: string, delay: number, action: () => void) {
      entries.set(key, { dueAt: Date.now() + Math.max(0, delay), action })
      arm()
    },
    cancel(key: string) {
      if (!entries.delete(key)) return
      arm()
    },
    cancelAll() {
      entries.clear()
      if (timer !== null) clearTimeout(timer)
      timer = null
    },
    pendingEntryCount: () => entries.size,
    pendingTimerCount: () => (timer === null ? 0 : 1),
  }
}

export type ChallengeRecoveryOwner = ReturnType<typeof createChallengeRecoveryOwner>
