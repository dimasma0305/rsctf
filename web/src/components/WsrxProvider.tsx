import { useLocalStorage } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import { mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { Wsrx, WsrxError, WsrxErrorKind, WsrxFeature, WsrxInstance, WsrxOptions, WsrxState } from '@xdsec/wsrx'
import { t } from 'i18next'
import { createContext, useCallback, use, useEffect, useMemo, useRef, useState } from 'react'
import { showErrorMsg } from '@Utils/Shared'
import { createWsrxReadinessScheduler } from '@Utils/WsrxReadiness'
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
  wsrxReadinessExpired: ReadonlySet<string>
  wsrxOptions: CustomWsrxOptions
  doWsrxConnect: () => void
  retryWsrxReadiness: (remote: string) => void
  setWsrxOptions: (options: CustomWsrxOptions | ((prev: CustomWsrxOptions) => CustomWsrxOptions)) => void
}

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
  const [wsrxReadinessExpired, setWsrxReadinessExpired] = useState<ReadonlySet<string>>(() => new Set())
  const platformConfig = useConfig()

  const [wsrxOptions, setWsrxOptions] = useLocalStorage<CustomWsrxOptions>({
    key: 'wsrx-options',
    defaultValue: DefaultWsrxOptions,
    getInitialValueInEffect: false,
  })

  const wsrx = useMemo(() => new Wsrx(getWsrxConfig(wsrxOptions)), [])
  const readinessScheduler = useMemo(
    () =>
      createWsrxReadinessScheduler({
        sync: () => wsrx.sync(),
        onExpiredChange: setWsrxReadinessExpired,
      }),
    [wsrx]
  )
  const connectGeneration = useRef(0)
  const connectActive = useRef(false)
  const pendingGeneration = useRef<number | null>(null)
  const retryAttempt = useRef(0)
  const retryTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const runConnect = useRef<(generation: number) => void>(() => undefined)

  runConnect.current = (generation: number) => {
    if (connectActive.current) {
      pendingGeneration.current = generation
      return
    }
    connectActive.current = true
    readinessScheduler.setEnabled(false)
    void (async () => {
      try {
        await wsrx.connect()
        if (generation !== connectGeneration.current) return
        if (wsrx.getState() === WsrxState.Usable) {
          await wsrx.sync()
          retryAttempt.current = 0
          readinessScheduler.setEnabled(true)
        }
      } catch (err) {
        if (generation !== connectGeneration.current) return
        readinessScheduler.setEnabled(false)
        if (err instanceof WsrxError && err.kind === WsrxErrorKind.DaemonUnavailable) {
          const attempt = Math.min(retryAttempt.current++, 5)
          const baseDelay = Math.min(8_000, 500 * 2 ** attempt)
          const delay = baseDelay + Math.floor(Math.random() * Math.max(1, baseDelay / 4))
          retryTimer.current = setTimeout(() => runConnect.current(generation), delay)
        } else {
          HandleWsrxError(err, t)
        }
      } finally {
        connectActive.current = false
        const pending = pendingGeneration.current
        pendingGeneration.current = null
        if (pending !== null && pending === connectGeneration.current) runConnect.current(pending)
      }
    })()
  }

  const doWsrxConnect = useCallback(() => {
    connectGeneration.current += 1
    retryAttempt.current = 0
    if (retryTimer.current !== null) clearTimeout(retryTimer.current)
    retryTimer.current = setTimeout(() => runConnect.current(connectGeneration.current), 100)
  }, [])

  useEffect(
    () => () => {
      connectGeneration.current += 1
      if (retryTimer.current !== null) clearTimeout(retryTimer.current)
      readinessScheduler.dispose()
    },
    [readinessScheduler]
  )

  useEffect(() => {
    readinessScheduler.reset()
    if (!wsrxOptions || platformConfig.config.portMapping !== ContainerPortMappingType.PlatformProxy) {
      connectGeneration.current += 1
      pendingGeneration.current = null
      if (retryTimer.current !== null) clearTimeout(retryTimer.current)
      return
    }

    wsrx.setOptions(getWsrxConfig(wsrxOptions))
    doWsrxConnect()
    return () => {
      connectGeneration.current += 1
      pendingGeneration.current = null
      if (retryTimer.current !== null) clearTimeout(retryTimer.current)
      readinessScheduler.reset()
    }
  }, [wsrx, wsrxOptions, doWsrxConnect, platformConfig.config.portMapping, readinessScheduler])

  useEffect(() => {
    if (platformConfig?.config.title) {
      const newName = platformConfig.config.title + '::CTF'
      setWsrxOptions((prevOptions) => {
        if (prevOptions.name === newName) return prevOptions
        return {
          ...prevOptions,
          name: newName,
        }
      })
    }
  }, [platformConfig?.config.title, setWsrxOptions])

  const updateState = useCallback(
    (newState: WsrxState) => {
      if (newState !== WsrxState.Usable) readinessScheduler.setEnabled(false)
      else if (!connectActive.current) readinessScheduler.setEnabled(true)
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
    },
    [readinessScheduler]
  )

  useEffect(() => {
    const id = wsrx.onStateChange(updateState)
    return () => wsrx.offStateChange(id)
  }, [wsrx, updateState])

  useEffect(() => {
    const updateInstances = (instances: WsrxInstance[]) => {
      setWsrxInstances([...instances])
      readinessScheduler.updatePending(
        instances.filter((instance) => instance.latency === -1).map((instance) => instance.remote)
      )
    }
    updateInstances(wsrx.list())
    const id = wsrx.onInstancesChange(updateInstances)
    return () => wsrx.offInstancesChange(id)
  }, [wsrx, readinessScheduler])

  const retryWsrxReadiness = useCallback((remote: string) => readinessScheduler.retry(remote), [readinessScheduler])

  const contextValue = useMemo(
    () => ({
      wsrx,
      wsrxState,
      wsrxInstances,
      wsrxReadinessExpired,
      wsrxOptions,
      doWsrxConnect,
      retryWsrxReadiness,
      setWsrxOptions,
    }),
    [
      wsrx,
      wsrxState,
      wsrxInstances,
      wsrxReadinessExpired,
      wsrxOptions,
      doWsrxConnect,
      retryWsrxReadiness,
      setWsrxOptions,
    ]
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
