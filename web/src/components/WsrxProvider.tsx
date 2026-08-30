import { useDebouncedCallback, useLocalStorage } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import { mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { Wsrx, WsrxError, WsrxErrorKind, WsrxFeature, WsrxInstance, WsrxOptions, WsrxState } from '@xdsec/wsrx'
import { t } from 'i18next'
import { createContext, useCallback, use, useEffect, useMemo, useRef, useState } from 'react'
import { showErrorMsg } from '@Utils/Shared'
import { useConfig } from '@Hooks/useConfig'
import { ContainerPortMappingType } from '@Api'

interface CustomWsrxOptions {
  name: string
  api: string
  allowLan: boolean
}

export const DefaultWsrxOptions: CustomWsrxOptions = {
  api: 'http://127.0.0.1:3307',
  name: 'RS::CTF',
  allowLan: false,
}

export const HandleWsrxError = (err: unknown, t: (key: string) => string) => {
  if (err instanceof WsrxError) {
    switch (err.kind) {
      case WsrxErrorKind.VersionMismatch:
        showNotification({
          id: 'wsrx-version-mismatch',
          color: 'orange',
          icon: <Icon path={mdiClose} size={1} />,
          title: t('wsrx.error.version_mismatch.title'),
          message: t('wsrx.error.version_mismatch.message'),
        })
        break
      case WsrxErrorKind.DaemonUnavailable:
        showNotification({
          id: 'wsrx-daemon-offline',
          color: 'red',
          icon: <Icon path={mdiClose} size={1} />,
          title: t('wsrx.error.daemon_unavailable.title'),
          message: t('wsrx.error.daemon_unavailable.message'),
        })
        break
      case WsrxErrorKind.DaemonError:
        showNotification({
          id: 'wsrx-daemon-error',
          color: 'red',
          icon: <Icon path={mdiClose} size={1} />,
          title: t('wsrx.error.daemon_error.title'),
          message: t('wsrx.error.daemon_error.message'),
        })
        break
      default:
        showNotification({
          id: 'wsrx-unknown-error',
          color: 'red',
          icon: <Icon path={mdiClose} size={1} />,
          title: t('common.error.unknown'),
          message: t('wsrx.error.unknown'),
        })
    }
  } else {
    showErrorMsg(err, t)
  }
}

interface WsrxContextType {
  wsrx: Wsrx
  wsrxState: WsrxState
  wsrxInstances: WsrxInstance[]
  wsrxOptions: CustomWsrxOptions
  doWsrxConnect: () => void
  applyWsrxOptions: (options: CustomWsrxOptions) => void
  watchPendingTunnel: (remote: string, onExpired: () => void) => () => void
}

interface PendingTunnelWatcher {
  deadline: number
  onExpired: () => void
}

const ACCELERATED_SYNC_INTERVAL_MS = 1_500
const ACCELERATED_SYNC_WINDOW_MS = 8_000

const WsrxContext = createContext<WsrxContextType | null>(null)

const getWsrxConfig = (options: CustomWsrxOptions) => {
  const config: WsrxOptions = {
    name: options.name ?? DefaultWsrxOptions.name,
    api: options.api ?? DefaultWsrxOptions.api,
    features: [WsrxFeature.Basic, WsrxFeature.Pingfall],
    settings: {
      pingfall: {
        status: [400, 404],
        drop_unknown: false,
      },
    },
  }

  return config
}

export const WsrxProvider: React.FC<React.PropsWithChildren> = ({ children }) => {
  const [wsrxState, setWsrxState] = useState<WsrxState>(WsrxState.Invalid)
  const [wsrxInstances, setWsrxInstances] = useState<WsrxInstance[]>([])
  const platformConfig = useConfig()
  const pendingTunnels = useRef(new Map<string, Map<symbol, PendingTunnelWatcher>>())
  const syncInFlight = useRef<Promise<void> | null>(null)
  const [pendingRevision, setPendingRevision] = useState(0)

  const [wsrxOptions, persistWsrxOptions] = useLocalStorage<CustomWsrxOptions>({
    key: 'wsrx-options',
    defaultValue: DefaultWsrxOptions,
    getInitialValueInEffect: false,
  })

  const wsrx = useMemo(() => new Wsrx(getWsrxConfig(wsrxOptions)), [])

  const cancelPendingTunnels = useCallback(() => {
    pendingTunnels.current.clear()
    setPendingRevision((revision) => revision + 1)
  }, [])

  const watchPendingTunnel = useCallback((remote: string, onExpired: () => void) => {
    if (!remote) return () => undefined
    const watcherId = Symbol(remote)
    const watchers = pendingTunnels.current.get(remote) ?? new Map<symbol, PendingTunnelWatcher>()
    watchers.set(watcherId, { deadline: Date.now() + ACCELERATED_SYNC_WINDOW_MS, onExpired })
    pendingTunnels.current.set(remote, watchers)
    setPendingRevision((revision) => revision + 1)
    return () => {
      const current = pendingTunnels.current.get(remote)
      current?.delete(watcherId)
      if (current?.size === 0) pendingTunnels.current.delete(remote)
      setPendingRevision((revision) => revision + 1)
    }
  }, [])

  const doWsrxConnect = useDebouncedCallback(async () => {
    try {
      // connect() rejects asynchronously when the optional local daemon is
      // unavailable. Await it so a normal "daemon not installed" state does
      // not become an unhandled rejection on every route.
      await wsrx.connect()
      if (wsrx.getState() === WsrxState.Usable) await wsrx.sync()
    } catch (err) {
      if (err instanceof WsrxError && err.kind !== WsrxErrorKind.DaemonUnavailable) HandleWsrxError(err, t)
    }
  }, 100)

  const applyWsrxOptions = useCallback(
    (options: CustomWsrxOptions) => {
      cancelPendingTunnels()
      wsrx.setOptions(getWsrxConfig(options))
      persistWsrxOptions(options)
      doWsrxConnect()
    },
    [cancelPendingTunnels, doWsrxConnect, persistWsrxOptions, wsrx]
  )

  useEffect(() => {
    if (pendingTunnels.current.size === 0) return
    if (wsrxState !== WsrxState.Usable) {
      pendingTunnels.current.forEach((watchers) => watchers.forEach((watcher) => watcher.onExpired()))
      pendingTunnels.current.clear()
      return
    }
    let cancelled = false
    let timer: ReturnType<typeof setTimeout> | null = null

    const settleWatchers = () => {
      const now = Date.now()
      const instances = wsrx.list()
      pendingTunnels.current.forEach((watchers, remote) => {
        if (instances.some((instance) => instance.remote === remote && instance.latency !== -1)) {
          pendingTunnels.current.delete(remote)
          return
        }
        watchers.forEach((watcher, watcherId) => {
          if (watcher.deadline > now) return
          watchers.delete(watcherId)
          watcher.onExpired()
        })
        if (watchers.size === 0) pendingTunnels.current.delete(remote)
      })
    }

    const run = async () => {
      settleWatchers()
      if (cancelled || pendingTunnels.current.size === 0) return
      try {
        if (!syncInFlight.current) {
          syncInFlight.current = wsrx.sync().finally(() => {
            syncInFlight.current = null
          })
        }
        await syncInFlight.current
      } catch {
        if (!cancelled) {
          pendingTunnels.current.forEach((watchers) => watchers.forEach((watcher) => watcher.onExpired()))
          pendingTunnels.current.clear()
        }
        return
      }
      settleWatchers()
      if (!cancelled && pendingTunnels.current.size > 0) {
        const nextDeadline = Math.min(
          ...Array.from(pendingTunnels.current.values()).flatMap((watchers) =>
            Array.from(watchers.values()).map((watcher) => watcher.deadline)
          )
        )
        const delay = Math.max(0, Math.min(ACCELERATED_SYNC_INTERVAL_MS, nextDeadline - Date.now()))
        timer = setTimeout(() => void run(), delay)
      }
    }

    void run()
    return () => {
      cancelled = true
      if (timer) clearTimeout(timer)
    }
  }, [pendingRevision, wsrx, wsrxState])

  useEffect(() => {
    if (!wsrxOptions || platformConfig.config.portMapping !== ContainerPortMappingType.PlatformProxy) return

    wsrx.setOptions(getWsrxConfig(wsrxOptions))
  }, [wsrx, wsrxOptions, platformConfig.config.portMapping])

  useEffect(() => {
    if (platformConfig?.config.title) {
      const newName = platformConfig.config.title + '::CTF'
      persistWsrxOptions((prevOptions) => {
        if (prevOptions.name === newName) return prevOptions
        return {
          ...prevOptions,
          name: newName,
        }
      })
    }
  }, [platformConfig?.config.title, persistWsrxOptions])

  const updateState = useCallback((newState: WsrxState) => {
    setWsrxState((prev) => {
      if (newState === WsrxState.Invalid && prev !== WsrxState.Invalid) {
        showNotification({
          id: 'wsrx-daemon-offline',
          color: 'red',
          icon: <Icon path={mdiClose} size={1} />,
          title: t('wsrx.error.daemon_offline.title'),
          message: t('wsrx.error.daemon_offline.message'),
        })
      }
      return newState
    })
  }, [])

  useEffect(() => {
    const id = wsrx.onStateChange(updateState)
    return () => wsrx.offStateChange(id)
  }, [wsrx, updateState])

  useEffect(() => {
    const updateInstances = (instances: WsrxInstance[]) => setWsrxInstances([...instances])
    updateInstances(wsrx.list())
    const id = wsrx.onInstancesChange(updateInstances)
    return () => wsrx.offInstancesChange(id)
  }, [wsrx])

  const contextValue = useMemo(
    () => ({
      wsrx,
      wsrxState,
      wsrxInstances,
      wsrxOptions,
      doWsrxConnect,
      applyWsrxOptions,
      watchPendingTunnel,
    }),
    [wsrx, wsrxState, wsrxInstances, wsrxOptions, doWsrxConnect, applyWsrxOptions, watchPendingTunnel]
  )

  return <WsrxContext.Provider value={contextValue}>{children}</WsrxContext.Provider>
}

export const useWsrx = () => {
  const context = use(WsrxContext)
  if (!context) {
    throw new Error('useWsrx must be used within a WsrxProvider')
  }
  return context
}
