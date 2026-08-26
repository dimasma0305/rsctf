import { getServerNowMilliseconds } from '@Utils/ServerClock'

type Timer = ReturnType<typeof setTimeout>

const TRANSIENT_STATUSES = new Set([408, 425, 429, 500, 502, 503, 504])
export const MAX_PROFILE_RETRIES = 5
const MAX_BACKOFF_MS = 30_000
const MAX_RETRY_AFTER_MS = 5 * 60_000

type ErrorWithResponse = {
  status?: unknown
  response?: {
    status?: unknown
    headers?: unknown
  }
}

export type ProfileErrorDisposition = 'anonymous' | 'banned' | 'retry' | 'stop'

export const httpErrorStatus = (error: unknown): number | null => {
  if (!error || typeof error !== 'object') return null
  const candidate = error as ErrorWithResponse
  const status = candidate.response?.status ?? candidate.status
  return typeof status === 'number' && Number.isInteger(status) ? status : null
}

export const profileErrorDisposition = (error: unknown): ProfileErrorDisposition => {
  if (error === null || error === undefined) return 'stop'
  const status = httpErrorStatus(error)
  if (status === 401) return 'anonymous'
  if (status === 403) return 'banned'
  if (status === null || TRANSIENT_STATUSES.has(status)) return 'retry'
  return 'stop'
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

/** Owns the sole pending retry so later errors and successful reads supersede it. */
export const createProfileRetryTimers = () => {
  let timer: Timer | null = null
  let generation = 0

  return {
    schedule(delay: number, action: () => void) {
      generation += 1
      const scheduledGeneration = generation
      if (timer !== null) clearTimeout(timer)
      timer = setTimeout(() => {
        timer = null
        if (generation === scheduledGeneration) action()
      }, delay)
    },
    cancel() {
      generation += 1
      if (timer !== null) clearTimeout(timer)
      timer = null
    },
    pending: () => (timer === null ? 0 : 1),
  }
}
