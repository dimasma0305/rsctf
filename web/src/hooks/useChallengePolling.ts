import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import useSWR from 'swr'
import {
  challengePollRetryDelay,
  createChallengePollOwner,
  createChallengeRecoveryOwner,
  isAbortError,
  isChallengePollRetryable,
  MAX_CHALLENGE_POLL_RETRIES,
  type ChallengeRecoveryOwner,
} from '@Utils/ChallengePolling'

interface ChallengePollingOptions<T> {
  key: string | null
  active: boolean
  refreshInterval: number | ((data: T | undefined) => number)
  request: (signal: AbortSignal) => Promise<T>
  revalidateOnFocus?: boolean
  revalidateOnReconnect?: boolean
  /** Share this owner across related detail/solver reads to retain one timer. */
  recoveryOwner?: ChallengeRecoveryOwner
  recoveryKey?: string
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
  recoveryOwner: sharedRecoveryOwner,
  recoveryKey,
}: ChallengePollingOptions<T>) => {
  const requestOwner = useMemo(createChallengePollOwner, [])
  const localRecoveryOwner = useMemo(createChallengeRecoveryOwner, [])
  const recoveryOwner = sharedRecoveryOwner ?? localRecoveryOwner
  const ownedRecoveryKey = recoveryKey ?? key ?? 'inactive-challenge-read'
  const activeRef = useRef(active)
  const failureCount = useRef(0)
  const [pausedKey, setPausedKey] = useState<string | null>(null)
  activeRef.current = active

  const cancel = useCallback(() => {
    requestOwner.cancel()
    recoveryOwner.cancel(ownedRecoveryKey)
  }, [ownedRecoveryKey, recoveryOwner, requestOwner])
  const fetcher = useCallback(async () => {
    const controller = requestOwner.begin()
    try {
      return await request(controller.signal)
    } finally {
      requestOwner.finish(controller)
    }
  }, [requestOwner, request])

  useEffect(() => {
    // Retry state belongs to one active key. Closing the surface or moving to
    // another challenge starts with a clean budget and no obsolete work.
    failureCount.current = 0
    setPausedKey(null)
    // Do not cancel in the new effect body: SWR may already have started this
    // key's first request. The previous effect cleanup owns cancellation for
    // the old key/closed modal, and unmount uses the same cleanup.
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
      recoveryOwner.cancel(ownedRecoveryKey)
    },
    onError: (error) => {
      if (!activeRef.current || isAbortError(error)) return
      failureCount.current += 1
      if (!isChallengePollRetryable(error) || failureCount.current >= MAX_CHALLENGE_POLL_RETRIES) {
        recoveryOwner.cancel(ownedRecoveryKey)
        setPausedKey(key)
      }
    },
    onErrorRetry: (error, _swrKey, config, revalidate, options) => {
      if (!activeRef.current || pausedKey === key || failureCount.current >= MAX_CHALLENGE_POLL_RETRIES) return
      const delay = challengePollRetryDelay(error, options.retryCount)
      if (delay === null) {
        recoveryOwner.cancel(ownedRecoveryKey)
        setPausedKey(key)
        return
      }
      recoveryOwner.schedule(ownedRecoveryKey, delay, () => {
        const ready =
          activeRef.current &&
          (config.refreshWhenHidden || config.isVisible()) &&
          (config.refreshWhenOffline || config.isOnline())
        if (!ready) return false
        void revalidate(options)
        return true
      })
    },
  })
}
