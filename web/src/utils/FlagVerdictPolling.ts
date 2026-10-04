import { AnswerResult } from '@Api'

type Timer = ReturnType<typeof setTimeout>

export interface FlagVerdictIdentity {
  gameId: number
  challengeId: number
  submissionId: number
}

interface ErrorWithResponse {
  status?: unknown
  response?: {
    status?: unknown
    headers?: unknown
  }
}

interface FlagVerdictPollerOptions {
  identity: FlagVerdictIdentity
  request: (identity: FlagVerdictIdentity, signal: AbortSignal) => Promise<AnswerResult>
  onTerminal: (identity: FlagVerdictIdentity, result: AnswerResult) => void
  onFailure: (identity: FlagVerdictIdentity, error: unknown) => void
  random?: () => number
}

const TRANSIENT_STATUSES = new Set([408, 425, 429, 500, 502, 503, 504])
const BASE_DELAY_MS = 1_000
export const MAX_FLAG_VERDICT_DELAY_MS = 10_000
export const MAX_FLAG_VERDICT_RETRY_AFTER_MS = 60_000
export const MAX_FLAG_VERDICT_FAILURES = 6

export const sameFlagVerdictIdentity = (left: FlagVerdictIdentity | null, right: FlagVerdictIdentity | null): boolean =>
  left === right ||
  (left !== null &&
    right !== null &&
    left.gameId === right.gameId &&
    left.challengeId === right.challengeId &&
    left.submissionId === right.submissionId)

const responseStatus = (error: unknown): number | null => {
  if (!error || typeof error !== 'object') return null
  const candidate = error as ErrorWithResponse
  const status = candidate.response?.status ?? candidate.status
  return typeof status === 'number' && Number.isInteger(status) ? status : null
}

const responseHeader = (error: unknown, name: string): string | null => {
  if (!error || typeof error !== 'object') return null
  const headers = (error as ErrorWithResponse).response?.headers
  if (!headers || typeof headers !== 'object') return null

  const getter = (headers as { get?: unknown }).get
  if (typeof getter === 'function') {
    const value = getter.call(headers, name)
    return typeof value === 'string' ? value : null
  }

  const record = headers as Record<string, unknown>
  const value = record[name] ?? record[name.toLowerCase()]
  if (typeof value === 'string') return value
  return typeof value === 'number' ? String(value) : null
}

const retryAfterMilliseconds = (error: unknown): number => {
  const value = Number(responseHeader(error, 'retry-after')?.trim())
  return Number.isFinite(value) && value >= 0 ? Math.min(MAX_FLAG_VERDICT_RETRY_AFTER_MS, value * 1_000) : 0
}

export const isRecoverableFlagVerdictError = (error: unknown): boolean => {
  const status = responseStatus(error)
  return status === null || TRANSIENT_STATUSES.has(status)
}

export const flagVerdictPendingDelay = (pendingReads: number): number =>
  Math.min(MAX_FLAG_VERDICT_DELAY_MS, BASE_DELAY_MS * 2 ** Math.min(4, Math.max(0, pendingReads)))

export const flagVerdictFailureDelay = (
  error: unknown,
  failures: number,
  random: () => number = Math.random
): number => {
  const exponent = Math.min(4, Math.max(0, failures - 1))
  const jitter = 0.8 + Math.min(1, Math.max(0, random())) * 0.4
  const backoff = Math.min(MAX_FLAG_VERDICT_DELAY_MS, Math.round(BASE_DELAY_MS * 2 ** exponent * jitter))
  return Math.max(backoff, retryAfterMilliseconds(error))
}

/**
 * Own one serialized verdict-recovery loop. The next read is scheduled only
 * after the previous read settles, and cancellation invalidates both its timer
 * and any in-flight request before either can publish a stale callback.
 */
export const createFlagVerdictPoller = ({
  identity,
  request,
  onTerminal,
  onFailure,
  random = Math.random,
}: FlagVerdictPollerOptions) => {
  let active = false
  let finished = false
  let generation = 0
  let timer: Timer | null = null
  let controller: AbortController | null = null
  let pendingReads = 0
  let failures = 0

  const cancelTimer = () => {
    if (timer !== null) clearTimeout(timer)
    timer = null
  }

  const stop = () => {
    active = false
    generation += 1
    cancelTimer()
    controller?.abort()
    controller = null
  }

  const finish = (callback: () => void) => {
    if (!active || finished) return
    finished = true
    stop()
    callback()
  }

  const schedule = (delay: number, expectedGeneration: number) => {
    if (!active || finished || generation !== expectedGeneration) return
    cancelTimer()
    timer = setTimeout(() => {
      timer = null
      if (active && !finished && generation === expectedGeneration) void poll(expectedGeneration)
    }, delay)
  }

  const poll = async (expectedGeneration: number): Promise<void> => {
    if (!active || finished || generation !== expectedGeneration || controller !== null) return

    const requestController = new AbortController()
    controller = requestController
    try {
      const result = await request(identity, requestController.signal)
      if (!active || finished || generation !== expectedGeneration || requestController.signal.aborted) return
      controller = null
      failures = 0

      if (result !== AnswerResult.FlagSubmitted) {
        finish(() => onTerminal(identity, result))
        return
      }

      const delay = flagVerdictPendingDelay(pendingReads)
      pendingReads += 1
      schedule(delay, expectedGeneration)
    } catch (error) {
      if (!active || finished || generation !== expectedGeneration || requestController.signal.aborted) return
      controller = null
      failures += 1
      if (!isRecoverableFlagVerdictError(error) || failures >= MAX_FLAG_VERDICT_FAILURES) {
        finish(() => onFailure(identity, error))
        return
      }
      schedule(flagVerdictFailureDelay(error, failures, random), expectedGeneration)
    }
  }

  return {
    start() {
      if (active || finished) return
      active = true
      generation += 1
      void poll(generation)
    },
    cancel() {
      if (finished) return
      finished = true
      stop()
    },
    pending: () => active && !finished,
  }
}
