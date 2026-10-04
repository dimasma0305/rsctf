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
