import { useEffect, useMemo } from 'react'
import { type SWRConfiguration } from 'swr'
import { httpErrorStatus, retryAfterMilliseconds } from '@Utils/ProfileRetry'

export const ADMIN_INSTANCE_STATS_CADENCE_MS = 10_000
export const ADMIN_INSTANCE_LIST_CADENCE_MS = 30_000
const MIN_RETRY_DELAY_MS = 1_000

const transientStatus = (status: number | null) =>
  status === null || status === 408 || status === 425 || status === 429 || (status >= 500 && status <= 599)

/** Permanent responses (especially 404) never receive an automatic retry. */
export const adminInstanceRetryDelay = (error: unknown, fallbackMs: number): number | null => {
  const status = httpErrorStatus(error)
  if (!transientStatus(status)) return null
  if (status !== 429) return fallbackMs
  return Math.max(MIN_RETRY_DELAY_MS, retryAfterMilliseconds(error) ?? fallbackMs)
}

const browserCanPoll = () =>
  (typeof document === 'undefined' || !document.hidden) &&
  (typeof navigator === 'undefined' || navigator.onLine !== false)

/**
 * Owns the sole error-recovery timer for the page's one batch request. SWR owns
 * the ordinary cadence; this owner makes 429 Retry-After and unmount cleanup
 * explicit without creating a timer per table row.
 */
export const createAdminInstancePollingConfig = (liveStats: boolean, canPoll: () => boolean = browserCanPoll) => {
  const cadence = liveStats ? ADMIN_INSTANCE_STATS_CADENCE_MS : ADMIN_INSTANCE_LIST_CADENCE_MS
  let retryTimer: ReturnType<typeof setTimeout> | null = null

  const cancelRetry = () => {
    if (retryTimer !== null) clearTimeout(retryTimer)
    retryTimer = null
  }

  const config: SWRConfiguration = {
    refreshInterval: cadence,
    refreshWhenHidden: false,
    refreshWhenOffline: false,
    revalidateOnFocus: true,
    revalidateOnReconnect: true,
    shouldRetryOnError: (error) => adminInstanceRetryDelay(error, cadence) !== null,
    onSuccess: cancelRetry,
    onDiscarded: cancelRetry,
    onErrorRetry: (error, _key, _config, revalidate, options) => {
      cancelRetry()
      const delay = adminInstanceRetryDelay(error, cadence)
      if (delay === null) return
      retryTimer = setTimeout(() => {
        retryTimer = null
        if (canPoll()) void revalidate(options)
      }, delay)
    },
  }

  return {
    config,
    cancel: cancelRetry,
    pendingRetries: () => (retryTimer === null ? 0 : 1),
  }
}

export const useAdminInstancePollingConfig = (liveStats: boolean) => {
  const owner = useMemo(() => createAdminInstancePollingConfig(liveStats), [liveStats])
  useEffect(() => () => owner.cancel(), [owner])
  return owner.config
}
