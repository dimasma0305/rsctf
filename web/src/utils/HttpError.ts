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

export const httpErrorHelp = (error: unknown): { key: string; fallback: string } | null => {
  const status = httpErrorStatus(error)
  if (status === 401) return { key: 'common.error.help.login', fallback: 'Sign in again, then retry.' }
  if (status === 403)
    return {
      key: 'common.error.help.permission',
      fallback: 'Check that you are using the right account, or ask the organizer for access.',
    }
  if (status === 404)
    return {
      key: 'common.error.help.missing',
      fallback: 'Go back to the list and open the item again. It may have been removed.',
    }
  if (status === 409)
    return { key: 'common.error.help.changed', fallback: 'Reload the latest details before trying again.' }
  if (status === 429) return { key: 'common.error.help.busy', fallback: 'Wait a moment before trying again.' }
  if (status !== null && status >= 500)
    return {
      key: 'common.error.help.server',
      fallback:
        'The server could not finish this request. Try again shortly; contact the organizer if it keeps happening.',
    }
  if (status === null && error && typeof error === 'object' && 'request' in error)
    return { key: 'common.error.help.connection', fallback: 'Check your internet connection, then try again.' }
  return null
}
