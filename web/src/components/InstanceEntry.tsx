import {
  ActionIcon,
  Anchor,
  Button,
  Divider,
  Group,
  SegmentedControl,
  Stack,
  Text,
  TextInput,
  Tooltip,
} from '@mantine/core'
import { useClipboard } from '@mantine/hooks'
import { useDebouncedCallback } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiContentCopy, mdiExclamation, mdiOpenInNew, mdiRefresh, mdiServerNetwork } from '@mdi/js'
import { Icon } from '@mdi/react'
import { WsrxState } from '@xdsec/wsrx'
import dayjs from 'dayjs'
import duration from 'dayjs/plugin/duration'
import { FC, useCallback, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { HandleWsrxError, useWsrx } from '@Components/WsrxProvider'
import { isInstanceExtensionWindowOpen, runInstanceExtension } from '@Utils/InstanceLifecycle'
import { getServerNowMilliseconds, useServerClockOffset, useServerClockTimeout, useServerNow } from '@Utils/ServerClock'
import { getProxyUrl as getProxyEntry } from '@Utils/Shared'
import {
  DEFAULT_PROXY_ENTRY_MODE,
  getLocalWsrxTunnelAction,
  getWsrxCapabilityBackoffMilliseconds,
  getWsrxCapabilityNextBatchDelay,
  getWsrxCapabilityRetryDelay,
  getWsrxListenerDrainDelay,
  getWsrxTunnelPhase,
  isRetryableWsrxCapabilityStatus,
  isWsrxReplacementReady,
  shouldDeletePreparedWsrxListener,
  shouldInvalidateWsrxCapability,
  shouldConnectLocalWsrx,
  shouldKeepWsrxListener,
  type ProxyEntryMode,
  type WsrxRefreshSource,
  WSRX_CAPABILITY_EXPIRY_MARGIN_MS,
} from '@Utils/WsrxTunnel'
import { useConfig } from '@Hooks/useConfig'
import api, { ClientFlagContext, ContainerPortMappingType } from '@Api'
import classes from '@Styles/InstanceEntry.module.css'
import misc from '@Styles/Misc.module.css'

dayjs.extend(duration)

const CAPABILITY_REFRESH_SAFETY_MS = 5 * 60 * 1000
const MAX_CAPABILITY_REQUEST_ATTEMPTS = 6
const MAX_TUNNEL_PREPARATION_ATTEMPTS = 4

interface InstanceEntryProps {
  test?: boolean
  /** Show real lifecycle controls even when the entry belongs to an admin test
   * container. The challenge editor normally owns these controls, while its
   * player-view preview delegates them to this component. */
  lifecycleControls?: boolean
  label?: string
  context: ClientFlagContext
  disabled?: boolean
  onCreate?: () => void
  onExtend?: () => void | Promise<void>
  onDestroy?: () => void
}

interface CountdownProps {
  time?: number | null
  onTimeout?: () => void
  extendEnabled: boolean
  enableExtend: () => void
}

const Countdown: FC<CountdownProps> = (props) => {
  const { time, onTimeout, extendEnabled, enableExtend } = props
  const { config } = useConfig()
  const now = useServerNow()
  const [timeoutExecuted, setTimeoutExecuted] = useState(false)
  const end = time ? dayjs(time) : now.add(config.defaultLifetime ?? 120, 'minutes')

  const countdown = dayjs.duration(end.diff(now))

  useEffect(() => {
    if (!extendEnabled && config.renewalWindow && countdown.asMinutes() < config.renewalWindow) enableExtend()

    const isExpired = countdown.asSeconds() <= 0
    if (isExpired && !timeoutExecuted && onTimeout) {
      setTimeoutExecuted(true)
      onTimeout()
    }

    if (!isExpired && timeoutExecuted) {
      setTimeoutExecuted(false)
    }
  }, [countdown, config.renewalWindow, timeoutExecuted, onTimeout])

  return (
    <Text span fw="bold">
      {countdown.asSeconds() > 0 ? countdown.format('HH:mm:ss') : '00:00:00'}
    </Text>
  )
}

export const InstanceEntry: FC<InstanceEntryProps> = (props) => {
  const { test: isPreview, label, context, disabled, onCreate, onDestroy } = props
  const showLifecycle = props.lifecycleControls ?? !isPreview
  const supportsExtend = !!props.onExtend
  const { wsrx, wsrxState, wsrxInstances, wsrxOptions, doWsrxConnect, watchPendingTunnel, scheduleTunnelDrain } =
    useWsrx()

  const { config } = useConfig()
  const clipBoard = useClipboard()

  const [withContainer, setWithContainer] = useState(!!context.instanceEntry)

  // Shared container: one container serves every team. Players can start/extend it but not
  // destroy it (admin-only), and on idle-expiry we just flip back to the start view locally.
  const isShared = context.isSharedInstance ?? false

  const instanceEntry = context.instanceEntry ?? ''
  const isPlatformProxy =
    config.portMapping === ContainerPortMappingType.PlatformProxy &&
    instanceEntry.length === 36 &&
    !instanceEntry.includes(':')

  const [canExtend, setCanExtend] = useState(false)
  const authoritativeClockOffset = useServerClockOffset()

  const { t } = useTranslation()

  const enableExtend = useDebouncedCallback(() => {
    showNotification({
      color: 'orange',
      title: t('challenge.notification.instance.extend.note.title'),
      message: t('challenge.notification.instance.extend.note.message'),
      icon: <Icon path={mdiExclamation} size={1} />,
    })
    setCanExtend(true)
  }, 100)

  useEffect(() => {
    setWithContainer(!!context.instanceEntry)
    const extensionWindowOpen = isInstanceExtensionWindowOpen(
      context.closeTime,
      config.renewalWindow ?? 10,
      getServerNowMilliseconds()
    )
    if (!extensionWindowOpen) enableExtend.cancel()
    setCanExtend(extensionWindowOpen)
  }, [authoritativeClockOffset, config.renewalWindow, context, enableExtend])

  const onExtend = async () => {
    if (!canExtend || !props.onExtend) return

    try {
      await runInstanceExtension(props.onExtend, () => {
        showNotification({
          color: 'teal',
          title: t('challenge.notification.instance.extend.success.title'),
          message: t('challenge.notification.instance.extend.success.message'),
          icon: <Icon path={mdiCheck} size={1} />,
        })
        setCanExtend(false)
      })
    } catch (err) {
      showNotification({
        color: 'red',
        title: t('challenge.notification.instance.extend.note.title'),
        message: (err as Error)?.message ?? t('common.error.unknown', 'An unknown error occurred'),
        icon: <Icon path={mdiExclamation} size={1} />,
      })
    }
  }

  // Platform-proxied instances can be used through the managed local WSRX
  // listener or by explicitly copying the short-lived WSS URL. Never present
  // the latter as though it were a netcat address.
  const [proxyEntryMode, setProxyEntryMode] = useState<ProxyEntryMode>(DEFAULT_PROXY_ENTRY_MODE)
  const [wsrxRemoteEntry, setWsrxRemoteEntry] = useState('')
  const [pendingWsrxRemoteEntry, setPendingWsrxRemoteEntry] = useState('')
  const [capabilityExpiresAt, setCapabilityExpiresAt] = useState<number | null>(null)
  const [pendingCapabilityExpiresAt, setPendingCapabilityExpiresAt] = useState<number | null>(null)
  const [tunnelRequestComplete, setTunnelRequestComplete] = useState(false)
  const [tunnelRequestFailed, setTunnelRequestFailed] = useState(false)
  const [tunnelCheckExpired, setTunnelCheckExpired] = useState(false)
  const [pendingTunnelCheckExpired, setPendingTunnelCheckExpired] = useState(false)
  const [tunnelRetrying, setTunnelRetrying] = useState(false)
  const localTraffic = wsrxInstances.find((traffic) => traffic.remote === wsrxRemoteEntry)
  const pendingTraffic = wsrxInstances.find((traffic) => traffic.remote === pendingWsrxRemoteEntry)
  const renewalOwner = useRef(false)
  const capabilityGeneration = useRef(0)
  const capabilityAbort = useRef<AbortController | null>(null)
  const renewalTimers = useRef(new Set<number>())
  const tunnelPreparationAttempts = useRef(0)
  const preparingRemoteEntry = useRef('')
  const componentMounted = useRef(true)
  const activeEntryGeneration = useRef(0)
  const remoteEntryRef = useRef(wsrxRemoteEntry)
  const capabilityExpiryRef = useRef(capabilityExpiresAt)
  const pendingRemoteEntryRef = useRef(pendingWsrxRemoteEntry)
  const activeLocalRef = useRef(localTraffic?.local)
  const pendingLocalRef = useRef(pendingTraffic?.local)
  const proxyModeRef = useRef(proxyEntryMode)
  const wsrxStateRef = useRef(wsrxState)
  const allowLanRef = useRef(wsrxOptions.allowLan)
  const launchCapabilityRequestRef = useRef<(source: WsrxRefreshSource, acquireOwnership?: boolean) => void>(() => {})
  remoteEntryRef.current = wsrxRemoteEntry
  capabilityExpiryRef.current = capabilityExpiresAt
  pendingRemoteEntryRef.current = pendingWsrxRemoteEntry
  activeLocalRef.current = localTraffic?.local
  pendingLocalRef.current = pendingTraffic?.local
  proxyModeRef.current = proxyEntryMode
  wsrxStateRef.current = wsrxState
  allowLanRef.current = wsrxOptions.allowLan

  const clearRenewalTimers = useCallback(() => {
    for (const timer of renewalTimers.current) window.clearTimeout(timer)
    renewalTimers.current.clear()
  }, [])

  const scheduleRenewalTimer = useCallback((callback: () => void, delayMilliseconds: number) => {
    const timer = window.setTimeout(
      () => {
        renewalTimers.current.delete(timer)
        callback()
      },
      Math.max(0, delayMilliseconds)
    )
    renewalTimers.current.add(timer)
  }, [])

  const waitForCapabilityRetry = useCallback((delayMilliseconds: number, signal: AbortSignal) => {
    return new Promise<void>((resolve) => {
      let timer = 0
      const finish = () => {
        window.clearTimeout(timer)
        signal.removeEventListener('abort', finish)
        resolve()
      }
      timer = window.setTimeout(finish, Math.max(0, delayMilliseconds))
      if (signal.aborted) finish()
      else signal.addEventListener('abort', finish, { once: true })
    })
  }, [])

  const drainLocalListener = useCallback(
    (local: string | undefined, expiresAt: number | null) => {
      if (!local) return
      const now = getServerNowMilliseconds()
      scheduleTunnelDrain(local, getWsrxListenerDrainDelay(now, expiresAt))
    },
    [scheduleTunnelDrain]
  )

  const commitCapability = useCallback(
    (remoteEntry: string, expiresAt: number, replacementReady: boolean, preparedLocal?: string) => {
      const oldRemoteEntry = remoteEntryRef.current
      const oldExpiresAt = capabilityExpiryRef.current
      const oldLocal = activeLocalRef.current
      activeEntryGeneration.current += 1
      remoteEntryRef.current = remoteEntry
      capabilityExpiryRef.current = expiresAt
      pendingRemoteEntryRef.current = ''
      preparingRemoteEntry.current = ''
      setWsrxRemoteEntry(remoteEntry)
      setCapabilityExpiresAt(expiresAt)
      setPendingWsrxRemoteEntry('')
      setPendingCapabilityExpiresAt(null)
      setPendingTunnelCheckExpired(false)
      setTunnelRequestComplete(replacementReady)
      setTunnelRequestFailed(false)
      setTunnelRetrying(false)
      tunnelPreparationAttempts.current = 0
      renewalOwner.current = false
      if (oldRemoteEntry && oldRemoteEntry !== remoteEntry) drainLocalListener(oldLocal, oldExpiresAt)
      if (preparedLocal && shouldDeletePreparedWsrxListener(replacementReady, preparedLocal, oldLocal)) {
        void wsrx.delete(preparedLocal).catch(() => undefined)
      }
    },
    [drainLocalListener, wsrx]
  )

  const launchCapabilityRequest = useCallback(
    (source: WsrxRefreshSource, acquireOwnership: boolean = true) => {
      if (!isPlatformProxy || !instanceEntry || (acquireOwnership && renewalOwner.current)) return
      if (acquireOwnership) {
        renewalOwner.current = true
        tunnelPreparationAttempts.current = 0
      }
      setTunnelRetrying(true)
      if (shouldConnectLocalWsrx({ mode: proxyModeRef.current, source, state: wsrxStateRef.current })) {
        doWsrxConnect()
      }

      const generation = ++capabilityGeneration.current
      const controller = new AbortController()
      capabilityAbort.current?.abort()
      capabilityAbort.current = controller

      const run = async () => {
        for (let attempt = 0; attempt < MAX_CAPABILITY_REQUEST_ATTEMPTS; attempt += 1) {
          try {
            const response = isPreview
              ? await api.proxy.proxyIssueNoInstanceCapability(instanceEntry, { signal: controller.signal })
              : await api.proxy.proxyIssueInstanceCapability(instanceEntry, { signal: controller.signal })
            if (controller.signal.aborted || generation !== capabilityGeneration.current) return

            const candidate = getProxyEntry(instanceEntry, isPreview, response.data.token)
            if (remoteEntryRef.current && proxyModeRef.current === 'wsrx') {
              pendingRemoteEntryRef.current = candidate
              setPendingWsrxRemoteEntry(candidate)
              setPendingCapabilityExpiresAt(response.data.expiresAt)
              setPendingTunnelCheckExpired(false)
            } else {
              commitCapability(candidate, response.data.expiresAt, false)
            }
            return
          } catch (err) {
            if (controller.signal.aborted || generation !== capabilityGeneration.current) return
            const failure = (err as { response?: { status?: number; headers?: unknown } })?.response
            const status = failure?.status
            const now = getServerNowMilliseconds()
            const oldExpiresAt = capabilityExpiryRef.current
            const oldPathStillValid = oldExpiresAt === null || now < oldExpiresAt
            const finalAttempt = attempt + 1 >= MAX_CAPABILITY_REQUEST_ATTEMPTS
            if (
              !isRetryableWsrxCapabilityStatus(status) ||
              !oldPathStillValid ||
              (finalAttempt && oldExpiresAt === null)
            ) {
              if (!remoteEntryRef.current) setTunnelRequestFailed(true)
              renewalOwner.current = false
              setTunnelRetrying(false)
              HandleWsrxError(err, t)
              return
            }

            const headers = failure?.headers as
              ({ get?: (name: string) => unknown } & Record<string, unknown>) | undefined
            const retryAfter = headers?.get?.('retry-after') ?? headers?.['retry-after']
            const latestRetryAt = oldExpiresAt === null ? null : oldExpiresAt - WSRX_CAPABILITY_EXPIRY_MARGIN_MS
            const retryDelay = getWsrxCapabilityRetryDelay(attempt, generation, retryAfter, now, latestRetryAt)
            if (retryDelay === null) {
              renewalOwner.current = false
              setTunnelRetrying(false)
              HandleWsrxError(err, t)
              return
            }
            if (finalAttempt && oldExpiresAt !== null) {
              // Keep one renewal owner across bounded request batches. A short
              // batch catches ordinary transient failures quickly; a 30-second
              // completion-scheduled pause then prevents a persistent outage
              // from turning the remaining capability window into request
              // churn. The old entry and listener stay untouched throughout.
              const nextBatchDelay = getWsrxCapabilityNextBatchDelay(now, oldExpiresAt, retryDelay)
              if (nextBatchDelay === null) {
                renewalOwner.current = false
                setTunnelRetrying(false)
                HandleWsrxError(err, t)
                return
              }
              scheduleRenewalTimer(() => {
                if (generation !== capabilityGeneration.current || !renewalOwner.current) return
                launchCapabilityRequestRef.current('automatic', false)
              }, nextBatchDelay)
              return
            }
            await waitForCapabilityRetry(retryDelay, controller.signal)
          }
        }
      }

      void run().finally(() => {
        if (capabilityAbort.current === controller) capabilityAbort.current = null
      })
    },
    [
      commitCapability,
      doWsrxConnect,
      instanceEntry,
      isPlatformProxy,
      isPreview,
      scheduleRenewalTimer,
      t,
      waitForCapabilityRetry,
    ]
  )
  launchCapabilityRequestRef.current = launchCapabilityRequest

  const retryPendingPreparation = useCallback(
    (pendingLocal?: string, err?: unknown) => {
      const now = getServerNowMilliseconds()
      const oldExpiresAt = capabilityExpiryRef.current
      const attempt = tunnelPreparationAttempts.current + 1
      tunnelPreparationAttempts.current = attempt
      pendingRemoteEntryRef.current = ''
      preparingRemoteEntry.current = ''
      setPendingWsrxRemoteEntry('')
      setPendingCapabilityExpiresAt(null)
      setPendingTunnelCheckExpired(false)
      if (pendingLocal) void wsrx.delete(pendingLocal).catch(() => undefined)
      if (err) HandleWsrxError(err, t)

      const oldPathStillValid = oldExpiresAt === null || now < oldExpiresAt
      if (!oldPathStillValid || attempt >= MAX_TUNNEL_PREPARATION_ATTEMPTS) {
        renewalOwner.current = false
        setTunnelRetrying(false)
        if (!remoteEntryRef.current) setTunnelRequestFailed(true)
        return
      }

      const generation = capabilityGeneration.current
      const retryDelay = getWsrxCapabilityBackoffMilliseconds(attempt, generation, undefined, now)
      scheduleRenewalTimer(() => {
        if (generation !== capabilityGeneration.current || !renewalOwner.current) return
        launchCapabilityRequest('automatic', false)
      }, retryDelay)
    },
    [launchCapabilityRequest, scheduleRenewalTimer, t, wsrx]
  )

  useEffect(() => {
    capabilityAbort.current?.abort()
    clearRenewalTimers()
    capabilityGeneration.current += 1
    activeEntryGeneration.current += 1
    renewalOwner.current = false
    if (pendingLocalRef.current) void wsrx.delete(pendingLocalRef.current).catch(() => undefined)
    drainLocalListener(activeLocalRef.current, capabilityExpiryRef.current)
    remoteEntryRef.current = ''
    capabilityExpiryRef.current = null
    pendingRemoteEntryRef.current = ''
    preparingRemoteEntry.current = ''
    setWsrxRemoteEntry('')
    setPendingWsrxRemoteEntry('')
    setCapabilityExpiresAt(null)
    setPendingCapabilityExpiresAt(null)
    setTunnelRequestComplete(false)
    setTunnelRequestFailed(false)
    setTunnelCheckExpired(false)
    setPendingTunnelCheckExpired(false)
    setTunnelRetrying(false)
    if (!isPlatformProxy || !instanceEntry) return

    renewalOwner.current = true
    launchCapabilityRequest('automatic', false)
  }, [clearRenewalTimers, drainLocalListener, instanceEntry, isPlatformProxy, launchCapabilityRequest, wsrx])

  useEffect(() => {
    componentMounted.current = true
    return () => {
      componentMounted.current = false
      capabilityAbort.current?.abort()
      clearRenewalTimers()
      capabilityGeneration.current += 1
      activeEntryGeneration.current += 1
      renewalOwner.current = false
      if (pendingLocalRef.current) void wsrx.delete(pendingLocalRef.current).catch(() => undefined)
      drainLocalListener(activeLocalRef.current, capabilityExpiryRef.current)
    }
  }, [clearRenewalTimers, drainLocalListener, wsrx])

  useEffect(() => {
    if (!pendingWsrxRemoteEntry || proxyEntryMode !== 'wsrx' || wsrxState !== WsrxState.Usable) return
    if (pendingLocalRef.current || preparingRemoteEntry.current === pendingWsrxRemoteEntry) return

    let active = true
    const requestedRemoteEntry = pendingWsrxRemoteEntry
    const requestedGeneration = capabilityGeneration.current
    const requestedAllowLan = wsrxOptions.allowLan
    preparingRemoteEntry.current = requestedRemoteEntry
    const prepare = async () => {
      try {
        const added = await wsrx.add({
          label,
          remote: requestedRemoteEntry,
          local: requestedAllowLan ? '0.0.0.0:0' : '127.0.0.1:0',
        })
        const stillPending =
          active &&
          pendingRemoteEntryRef.current === requestedRemoteEntry &&
          capabilityGeneration.current === requestedGeneration
        const transferredToActive = remoteEntryRef.current === requestedRemoteEntry
        if (
          !shouldKeepWsrxListener({
            mounted: componentMounted.current,
            ownerCurrent: stillPending || transferredToActive,
            mode: proxyModeRef.current,
            state: wsrxStateRef.current,
            allowLan: allowLanRef.current,
            requestedAllowLan,
          })
        ) {
          await wsrx.delete(added.local).catch(() => undefined)
        }
      } catch (err) {
        if (active && pendingRemoteEntryRef.current === requestedRemoteEntry) {
          retryPendingPreparation(pendingLocalRef.current, err)
        }
      } finally {
        if (preparingRemoteEntry.current === requestedRemoteEntry) preparingRemoteEntry.current = ''
      }
    }

    void prepare()
    return () => {
      active = false
    }
  }, [label, pendingWsrxRemoteEntry, proxyEntryMode, retryPendingPreparation, wsrx, wsrxOptions.allowLan, wsrxState])

  useEffect(() => {
    setPendingTunnelCheckExpired(false)
    if (!pendingWsrxRemoteEntry || proxyEntryMode !== 'wsrx' || wsrxState !== WsrxState.Usable) return
    return watchPendingTunnel(pendingWsrxRemoteEntry, () => setPendingTunnelCheckExpired(true))
  }, [pendingWsrxRemoteEntry, proxyEntryMode, watchPendingTunnel, wsrxState])

  useEffect(() => {
    if (!pendingWsrxRemoteEntry || pendingCapabilityExpiresAt === null || pendingTunnelCheckExpired) return
    if (proxyEntryMode === 'wsrx' && !isWsrxReplacementReady(pendingTraffic)) return
    commitCapability(
      pendingWsrxRemoteEntry,
      pendingCapabilityExpiresAt,
      proxyEntryMode === 'wsrx',
      pendingTraffic?.local
    )
  }, [
    commitCapability,
    pendingCapabilityExpiresAt,
    pendingTraffic,
    pendingTunnelCheckExpired,
    pendingWsrxRemoteEntry,
    proxyEntryMode,
  ])

  useEffect(() => {
    if (!pendingWsrxRemoteEntry || !pendingTunnelCheckExpired || wsrxState !== WsrxState.Usable) return
    retryPendingPreparation(pendingTraffic?.local)
  }, [pendingTraffic?.local, pendingTunnelCheckExpired, pendingWsrxRemoteEntry, retryPendingPreparation, wsrxState])

  useEffect(() => {
    const requestedRemoteEntry = wsrxRemoteEntry
    const requestedGeneration = activeEntryGeneration.current
    const requestedAllowLan = wsrxOptions.allowLan
    const existingLocal = activeLocalRef.current
    const action = getLocalWsrxTunnelAction({
      mode: proxyEntryMode,
      state: wsrxState,
      remoteEntry: requestedRemoteEntry,
      localEntry: existingLocal,
      allowLan: requestedAllowLan,
    })
    if (action === 'idle') return
    if (action === 'reuse') {
      setTunnelRequestComplete(true)
      setTunnelRequestFailed(false)
      return
    }

    const localAddr = requestedAllowLan ? '0.0.0.0:0' : '127.0.0.1:0'
    let active = true
    setTunnelRequestComplete(false)
    setTunnelRequestFailed(false)

    const requestProxy = async () => {
      try {
        if (action === 'rebind' && existingLocal) {
          await wsrx.delete(existingLocal)
          if (
            !shouldKeepWsrxListener({
              mounted: componentMounted.current,
              ownerCurrent:
                active &&
                activeEntryGeneration.current === requestedGeneration &&
                remoteEntryRef.current === requestedRemoteEntry,
              mode: proxyModeRef.current,
              state: wsrxStateRef.current,
              allowLan: allowLanRef.current,
              requestedAllowLan,
            })
          )
            return
        }

        const added = await wsrx.add({
          label,
          remote: requestedRemoteEntry,
          local: localAddr,
        })
        if (
          !shouldKeepWsrxListener({
            mounted: componentMounted.current,
            ownerCurrent:
              active &&
              activeEntryGeneration.current === requestedGeneration &&
              remoteEntryRef.current === requestedRemoteEntry,
            mode: proxyModeRef.current,
            state: wsrxStateRef.current,
            allowLan: allowLanRef.current,
            requestedAllowLan,
          })
        ) {
          await wsrx.delete(added.local).catch(() => undefined)
          return
        }
        setTunnelRequestComplete(true)
      } catch (err) {
        if (active && componentMounted.current && remoteEntryRef.current === requestedRemoteEntry) {
          setTunnelRequestComplete(true)
          setTunnelRequestFailed(true)
          HandleWsrxError(err, t)
        }
      }
    }

    requestProxy()
    return () => {
      active = false
    }
  }, [wsrx, wsrxRemoteEntry, wsrxState, label, proxyEntryMode, t, wsrxOptions.allowLan])

  useEffect(() => {
    setTunnelCheckExpired(false)
    if (!localTraffic || localTraffic.latency !== -1) return

    // One provider-owned completion scheduler serves every pending entry and
    // falls back to the daemon library's ordinary sync after this bounded window.
    return watchPendingTunnel(wsrxRemoteEntry, () => setTunnelCheckExpired(true))
  }, [localTraffic?.latency, localTraffic?.local, watchPendingTunnel, wsrxRemoteEntry])

  const phase = getWsrxTunnelPhase({
    isPlatformProxy,
    wsrxState,
    remoteEntry: wsrxRemoteEntry,
    traffic: localTraffic,
    requestComplete: tunnelRequestComplete,
    checkExpired: tunnelCheckExpired,
    requestFailed: tunnelRequestFailed,
  })

  const localEntry = phase === 'ready' ? (localTraffic?.local ?? '') : ''
  const isWssMode = isPlatformProxy && proxyEntryMode === 'wss'
  const entry = isPlatformProxy ? (isWssMode ? wsrxRemoteEntry : localEntry) : instanceEntry
  const canUseEntry = !!entry
  const canOpenEntry = canUseEntry && !isWssMode

  const onRefreshProxyEntry = useCallback(
    (source: WsrxRefreshSource) => launchCapabilityRequest(source),
    [launchCapabilityRequest]
  )

  useServerClockTimeout(
    () => void onRefreshProxyEntry('automatic'),
    isPlatformProxy ? capabilityExpiresAt : null,
    CAPABILITY_REFRESH_SAFETY_MS,
    1000
  )

  const invalidateExpiredProxyCapability = useCallback(() => {
    if (
      capabilityExpiresAt === null ||
      !shouldInvalidateWsrxCapability(getServerNowMilliseconds(), capabilityExpiresAt, capabilityExpiryRef.current)
    )
      return

    const expiredLocal = activeLocalRef.current
    capabilityExpiryRef.current = null
    remoteEntryRef.current = ''
    setWsrxRemoteEntry('')
    setCapabilityExpiresAt(null)
    setTunnelRequestComplete(false)
    setTunnelRequestFailed(true)
    setTunnelCheckExpired(false)
    drainLocalListener(expiredLocal, capabilityExpiresAt)
  }, [capabilityExpiresAt, drainLocalListener])

  useServerClockTimeout(invalidateExpiredProxyCapability, isPlatformProxy ? capabilityExpiresAt : null, 0, 0)

  const tunnelStatusColor = phase === 'ready' ? 'green' : phase === 'unhealthy' ? 'red' : 'orange'

  const onCopyEntry = () => {
    if (!canUseEntry) return
    clipBoard.copy(entry)

    showNotification({
      color: 'teal',
      message: isWssMode ? t('wsrx.notification.url_copied') : t('challenge.notification.instance.copied.entry'),
      icon: <Icon path={mdiCheck} size={1} />,
    })
  }

  const onOpenEntry = () => {
    if (!canOpenEntry) return

    const webEntry = isPlatformProxy && wsrxOptions.allowLan ? entry.replace('0.0.0.0', '127.0.0.1') : entry
    window.open(`http://${webEntry}`, '_blank', 'noopener,noreferrer')
  }

  if (!withContainer) {
    return !showLifecycle ? (
      <Text size="md" c="dimmed" fw="bold" pt={30}>
        {t('challenge.content.instance.test.no_container')}
      </Text>
    ) : (
      <Group justify="space-between" wrap="nowrap">
        <Stack align="left" gap={0}>
          <Text size="sm" fw="bold">
            {t('challenge.content.instance.no_container.message')}
          </Text>
          <Text size="xs" c="dimmed" fw="bold">
            {t('challenge.content.instance.no_container.note', {
              min: config.defaultLifetime,
            })}
          </Text>
        </Stack>

        <Button onClick={onCreate} disabled={disabled} loading={disabled} data-guide="instance-start">
          {t('challenge.button.instance.create')}
        </Button>
      </Group>
    )
  }

  return (
    <Stack gap="sm" w="100%">
      <TextInput
        data-guide="instance-entry"
        label={
          <Text size="sm" fw="bold">
            {t('challenge.content.instance.entry.label')}
          </Text>
        }
        description={
          isPlatformProxy && (
            <Stack gap="xs" data-guide="wsrx-setup">
              <SegmentedControl
                data-guide="wsrx-local-mode"
                data-guide-value="wsrx"
                value={proxyEntryMode}
                onChange={(value) => {
                  const mode = value as ProxyEntryMode
                  setProxyEntryMode(mode)
                  if (shouldConnectLocalWsrx({ mode, source: 'player', state: wsrxState })) doWsrxConnect()
                }}
                data={[
                  {
                    label: t('wsrx.mode.local'),
                    value: 'wsrx',
                  },
                  { label: t('wsrx.mode.wss'), value: 'wss' },
                ]}
                aria-label={t('wsrx.mode.label')}
                size="xs"
                fullWidth
              />
              {proxyEntryMode === 'wsrx' ? (
                <>
                  <Text span size="sm">
                    {t('wsrx.tunnel.description')}&nbsp;
                    <Anchor
                      href="https://github.com/XDSEC/WebSocketReflectorX/releases"
                      target="_blank"
                      rel="noreferrer"
                      data-guide="wsrx-download"
                    >
                      {t('challenge.content.instance.entry.description.anchor')}
                    </Anchor>
                  </Text>
                </>
              ) : (
                <Text size="sm">{t('wsrx.mode.wss_description')}</Text>
              )}
              {proxyEntryMode === 'wsrx' && (
                <Text size="xs" c={tunnelStatusColor} role="status" aria-live="polite">
                  {t(`wsrx.tunnel.${phase}`)}
                </Text>
              )}
            </Stack>
          )
        }
        descriptionProps={isPlatformProxy ? { component: 'div' } : undefined}
        leftSection={
          <Icon path={mdiServerNetwork} size={1} data-proxied={canUseEntry || undefined} className={classes.icon} />
        }
        value={entry}
        placeholder={
          isPlatformProxy
            ? proxyEntryMode === 'wss'
              ? t('wsrx.mode.wss_placeholder')
              : t('wsrx.tunnel.placeholder')
            : undefined
        }
        readOnly
        classNames={{ input: misc.ffmono }}
        rightSection={
          <Group gap={2} wrap="nowrap">
            <Divider orientation="vertical" pr={4} />
            {isPlatformProxy && (
              <Tooltip
                label={proxyEntryMode === 'wsrx' ? t('wsrx.button.retry_tunnel') : t('wsrx.button.refresh_url')}
                withArrow
              >
                <ActionIcon
                  aria-label={proxyEntryMode === 'wsrx' ? t('wsrx.button.retry_tunnel') : t('wsrx.button.refresh_url')}
                  onClick={() => void onRefreshProxyEntry('player')}
                  loading={tunnelRetrying}
                >
                  <Icon path={mdiRefresh} size={1} />
                </ActionIcon>
              </Tooltip>
            )}
            <Tooltip label={t('common.button.copy')} withArrow>
              <ActionIcon
                aria-label={t('common.button.copy')}
                onClick={onCopyEntry}
                disabled={!canUseEntry}
                data-guide="instance-copy"
                data-entry-mode={isPlatformProxy ? proxyEntryMode : 'direct'}
              >
                <Icon path={mdiContentCopy} size={1} />
              </ActionIcon>
            </Tooltip>
            <Tooltip label={t('challenge.content.instance.open.web')} withArrow>
              <ActionIcon
                aria-label={t('challenge.content.instance.open.web')}
                disabled={!canOpenEntry}
                onClick={onOpenEntry}
              >
                <Icon path={mdiOpenInNew} size={1} />
              </ActionIcon>
            </Tooltip>
          </Group>
        }
        rightSectionWidth={isPlatformProxy ? '7.75rem' : '5rem'}
      />
      {showLifecycle && (
        <Group justify="space-between" wrap="nowrap">
          <Stack align="left" gap={0}>
            <Text size="sm" fw={600}>
              {t('challenge.content.instance.actions.count_down')}
              <Countdown
                time={context.closeTime}
                extendEnabled={!supportsExtend || canExtend}
                enableExtend={enableExtend}
                onTimeout={isShared ? () => setWithContainer(false) : onDestroy}
              />
            </Text>
            <Text size="xs" c="dimmed" fw={600}>
              {isShared
                ? t('challenge.content.instance.shared.note', 'Shared by all teams — only an admin can stop it.')
                : t('challenge.content.instance.actions.note', { min: config.renewalWindow })}
            </Text>
          </Stack>
          <Group justify="right" wrap="nowrap" gap="xs">
            {supportsExtend && (
              <Button color="orange" onClick={onExtend} disabled={!canExtend || disabled} loading={disabled}>
                {t('challenge.button.instance.extend')}
              </Button>
            )}
            {!isShared && (
              <Button color="red" onClick={onDestroy} disabled={disabled} loading={disabled}>
                {t('challenge.button.instance.destroy')}
              </Button>
            )}
          </Group>
        </Group>
      )}
    </Stack>
  )
}
