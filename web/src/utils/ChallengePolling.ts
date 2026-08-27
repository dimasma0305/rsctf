import { httpErrorStatus, retryAfterMilliseconds } from '@Utils/ProfileRetry'

export const MAX_CHALLENGE_POLL_RETRIES = 3
export const MAX_CHALLENGE_RETRY_AFTER_MS = 5 * 60_000
const MAX_CHALLENGE_BACKOFF_MS = 30_000

type ErrorWithCode = {
  code?: unknown
  name?: unknown
}

export class NonJsonResponseError extends Error {
  readonly status: number

  constructor(status: number, contentType: string | null) {
    super(`Expected a JSON response, received ${contentType || 'an unknown content type'}`)
    this.name = 'NonJsonResponseError'
    this.status = status
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
    throw new NonJsonResponseError(response.status, typeof contentType === 'string' ? contentType : null)
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
