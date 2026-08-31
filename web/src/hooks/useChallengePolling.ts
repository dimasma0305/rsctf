import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import useSWR from 'swr'
import {
  challengePollRetryDelay,
  createChallengePollOwner,
  isAbortError,
  isChallengePollRetryable,
  MAX_CHALLENGE_POLL_RETRIES,
} from '@Utils/ChallengePolling'

interface ChallengePollingOptions<T> {
  key: string | null
  active: boolean
  refreshInterval: number | ((data: T | undefined) => number)
  request: (signal: AbortSignal) => Promise<T>
  revalidateOnFocus?: boolean
  revalidateOnReconnect?: boolean
}

/**
 * Own a modal-scoped request, retry timer, and refresh schedule. Closing the
 * modal removes the SWR key and aborts both current work and deferred recovery.
 */
export const useChallengePolling = <T>({
  key,
  active,
  refreshInterval,
  request,
  revalidateOnFocus = true,
  revalidateOnReconnect = true,
}: ChallengePollingOptions<T>) => {
  const owner = useMemo(createChallengePollOwner, [])
  const activeRef = useRef(active)
  const failureCount = useRef(0)
  const [pausedKey, setPausedKey] = useState<string | null>(null)
  activeRef.current = active

  const cancel = useCallback(() => owner.cancel(), [owner])
  const fetcher = useCallback(async () => {
    const controller = owner.begin()
    try {
      return await request(controller.signal)
    } finally {
      owner.finish(controller)
    }
  }, [owner, request])

  useEffect(() => {
    // Retry state belongs to one active key. Closing the surface or moving to
    // another challenge starts with a clean budget and no obsolete work.
    failureCount.current = 0
    setPausedKey(null)
    cancel()
    return cancel
  }, [active, cancel, key])

  // Keep a failed active key mounted so SWR retains the exact error for the UI.
  // The zero refresh cadence plus disabled focus/reconnect recovery below stop
  // terminal failures; only closing/reopening or changing keys clears them.
  const liveKey = active && key ? key : null
  return useSWR<T>(liveKey, fetcher, {
    // An error owns the sole recovery timer below. Suppressing the ordinary
    // cadence meanwhile is what makes Retry-After a real lower bound.
    refreshInterval: (data) => {
      if (!active || failureCount.current !== 0) return 0
      return typeof refreshInterval === 'function' ? refreshInterval(data) : refreshInterval
    },
    refreshWhenHidden: false,
    refreshWhenOffline: false,
    revalidateOnFocus: revalidateOnFocus && pausedKey !== key,
    revalidateOnReconnect: revalidateOnReconnect && pausedKey !== key,
    shouldRetryOnError: isChallengePollRetryable,
    onSuccess: () => {
      failureCount.current = 0
      setPausedKey((paused) => (paused === key ? null : paused))
      // A focus/reconnect revalidation can recover before the owned backoff
      // expires. Do not let that stale timer create one extra request later.
      owner.cancel()
    },
    onError: (error) => {
      if (!activeRef.current || isAbortError(error)) return
      failureCount.current += 1
      if (!isChallengePollRetryable(error) || failureCount.current >= MAX_CHALLENGE_POLL_RETRIES) {
        owner.cancel()
        setPausedKey(key)
      }
    },
    onErrorRetry: (error, _swrKey, config, revalidate, options) => {
      if (!activeRef.current || pausedKey === key || failureCount.current >= MAX_CHALLENGE_POLL_RETRIES) return
      const delay = challengePollRetryDelay(error, options.retryCount)
      if (delay === null) {
        owner.cancel()
        setPausedKey(key)
        return
      }
      owner.schedule(delay, () => {
        if (
          activeRef.current &&
          (config.refreshWhenHidden || config.isVisible()) &&
          (config.refreshWhenOffline || config.isOnline())
        ) {
          void revalidate(options)
        }
      })
    },
  })
}
