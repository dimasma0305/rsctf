import { useCallback, useEffect, useRef } from 'react'
import { useRecoveringHub } from '@Hooks/useRecoveringHub'

export const SCOREBOARD_PUSH_DEBOUNCE_MS = 250

/**
 * Wake the active scoreboard from the public per-game event stream. The event
 * contains no score data: HTTP remains authoritative, and the existing bounded
 * poller repairs a dropped event or unavailable WebSocket connection.
 */
export const useScoreboardLiveRefresh = (gameId: number, active: boolean, revalidate: () => Promise<unknown>) => {
  const revalidateRef = useRef(revalidate)
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)

  useEffect(() => {
    revalidateRef.current = revalidate
  }, [revalidate])

  const queueRefresh = useCallback(() => {
    if (timer.current !== null) return
    timer.current = setTimeout(() => {
      timer.current = null
      void revalidateRef.current().catch(() => undefined)
    }, SCOREBOARD_PUSH_DEBOUNCE_MS)
  }, [])

  useEffect(
    () => () => {
      if (timer.current !== null) clearTimeout(timer.current)
      timer.current = null
    },
    []
  )

  useRecoveringHub({
    active: active && gameId > 0,
    url: `/hub/user?game=${gameId}`,
    ownerKey: gameId,
    handlers: { ReceivedScoreboardChanged: queueRefresh },
    revalidate,
    // The scoreboard hooks already own their completion-scheduled HTTP
    // fallback. Do not create a second periodic request owner here.
    pollingIntervalMs: 0,
  })
}
