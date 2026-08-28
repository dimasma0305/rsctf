type HttpError = {
  status?: unknown
  response?: { status?: unknown }
}

export const httpErrorStatus = (error: unknown): number | null => {
  if (!error || typeof error !== 'object') return null
  const candidate = error as HttpError
  const status = candidate.response?.status ?? candidate.status
  return typeof status === 'number' && Number.isInteger(status) ? status : null
}

/** Transport failures, throttling, and every server-side failure can recover. */
export const isRetryableHttpError = (error: unknown) => {
  const status = httpErrorStatus(error)
  return status === null || status === 408 || status === 425 || status === 429 || status >= 500
}

type ErrorWithHeaders = HttpError & {
  response?: {
    status?: unknown
    headers?: unknown
  }
}

const headerValue = (headers: unknown, name: string): string | null => {
  if (!headers || typeof headers !== 'object') return null
  const candidate = headers as { get?: (key: string) => unknown; [key: string]: unknown }
  const value = typeof candidate.get === 'function' ? candidate.get(name) : candidate[name] ?? candidate[name.toLowerCase()]
  return typeof value === 'string' ? value : typeof value === 'number' ? String(value) : null
}

export const retryAfterMilliseconds = (error: unknown, now: number = Date.now()): number | null => {
  if (!error || typeof error !== 'object') return null
  const raw = headerValue((error as ErrorWithHeaders).response?.headers, 'retry-after')
  if (!raw) return null
  const seconds = Number(raw)
  if (Number.isFinite(seconds) && seconds >= 0) return Math.ceil(seconds * 1_000)
  const date = Date.parse(raw)
  return Number.isFinite(date) ? Math.max(0, date - now) : null
}

export const boundedRetryDelay = (
  error: unknown,
  retryCount: number,
  random: () => number = Math.random,
  now: number = Date.now()
): number | null => {
  if (!isRetryableHttpError(error) || retryCount >= 3) return null
  const retryAfter = retryAfterMilliseconds(error, now)
  if (retryAfter !== null) return Math.min(5 * 60_000, Math.max(250, retryAfter))
  const ceiling = Math.min(30_000, 1_000 * 2 ** Math.max(0, retryCount))
  return Math.max(250, Math.round(ceiling * (0.5 + Math.min(1, Math.max(0, random())) * 0.5)))
}
