import { isRetryableHttpError } from '@Utils/HttpError'

/**
 * A confirmation link is its own stable operation identity. Retry that exact
 * credential once after an ambiguous transport/server response so the durable
 * terminal result wins over a misleading "invalid link" error.
 */
export const reconcileAccountLink = async <T>(request: () => Promise<T>, signal: AbortSignal): Promise<T> => {
  try {
    return await request()
  } catch (error) {
    if (signal.aborted || !isRetryableHttpError(error)) throw error
    return request()
  }
}
