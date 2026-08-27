import { HubConnectionBuilder, JsonHubProtocol, LogLevel } from '@microsoft/signalr'
import { useCallback, useEffect, useRef, useState } from 'react'
import {
  CappedJitterRetryPolicy,
  configureHubTimeouts,
  HUB_EXHAUSTED_RETRY_MS,
  HubRecoveryController,
  type HubRecoveryState,
} from '@Utils/SignalRRecovery'

type HubHandler = (...arguments_: unknown[]) => void

interface RecoveringHubOptions {
  active: boolean
  url: string
  ownerKey?: string | number
  handlers: Record<string, HubHandler>
  revalidate: () => void | Promise<unknown>
  pollingIntervalMs: number
  onConnected?: (recovered: boolean) => void
  onExhausted?: (error?: unknown) => void
}

const browserCanPoll = () => document.visibilityState !== 'hidden' && navigator.onLine

/** Route-scoped SignalR owner shared by notices and operator feeds. Callback
 * refs may change as filters or translations revalidate, but the transport is
 * owned only by stable active/URL/owner-key lifecycle primitives. */
export const useRecoveringHub = ({
  active,
  url,
  ownerKey,
  handlers,
  revalidate,
  pollingIntervalMs,
  onConnected,
  onExhausted,
}: RecoveringHubOptions) => {
  const handlersRef = useRef(handlers)
  const revalidateRef = useRef(revalidate)
  const onConnectedRef = useRef(onConnected)
  const onExhaustedRef = useRef(onExhausted)
  const stopPromise = useRef<Promise<void>>(Promise.resolve())
  const waitForStop = useCallback(() => stopPromise.current, [])

  // Publish callback changes only after commit. Mutating these refs during a
  // concurrent render could make the still-live route deliver a message into
  // an abandoned render or the next route before its cleanup has run.
  useEffect(() => {
    handlersRef.current = handlers
    revalidateRef.current = revalidate
    onConnectedRef.current = onConnected
    onExhaustedRef.current = onExhausted
  })

  const [state, setState] = useState<HubRecoveryState>('idle')

  useEffect(() => {
    if (!active) {
      setState('idle')
      return
    }

    const connection = configureHubTimeouts(
      new HubConnectionBuilder()
        .withUrl(url)
        .withHubProtocol(new JsonHubProtocol())
        .withAutomaticReconnect(new CappedJitterRetryPolicy())
        .configureLogging(LogLevel.None)
        .build()
    )

    let disposed = false

    // Event names are a protocol surface and remain stable for this route.
    // Handler bodies are read from refs so UI/filter re-renders never restart
    // the socket or introduce a handshake gap.
    for (const name of Object.keys(handlersRef.current)) {
      connection.on(name, (...arguments_: unknown[]) => {
        if (!disposed) handlersRef.current[name]?.(...arguments_)
      })
    }

    const controller = new HubRecoveryController(connection, {
      revalidate: () => revalidateRef.current(),
      onConnected: (_generation, recovered) => onConnectedRef.current?.(recovered),
      onExhausted: (error) => onExhaustedRef.current?.(error),
      onStateChange: (next) => {
        if (!disposed) setState(next)
      },
      pollingIntervalMs,
      exhaustedRetryMs: HUB_EXHAUSTED_RETRY_MS,
      isPollingAllowed: browserCanPoll,
    })

    const resume = () => {
      if (!browserCanPoll()) return
      void controller.revalidateNow()
      if (controller.currentState === 'exhausted' && controller.canRetryAutomatically) controller.retryNow()
    }
    document.addEventListener('visibilitychange', resume)
    window.addEventListener('online', resume)
    controller.start()

    return () => {
      disposed = true
      document.removeEventListener('visibilitychange', resume)
      window.removeEventListener('online', resume)
      stopPromise.current = controller.stop()
    }
    // Deliberately exclude callbacks and full game/filter objects: refs keep
    // their behavior current while active, URL, and an optional primitive key
    // own the transport lifecycle.
  }, [active, ownerKey, pollingIntervalMs, url])

  return { state, waitForStop }
}
