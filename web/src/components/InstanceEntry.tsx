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
  getWsrxCapabilityRetryAt,
  getWsrxTunnelPhase,
  isLatestWsrxCapabilityRequest,
  shouldInvalidateWsrxCapability,
  shouldConnectLocalWsrx,
  type ProxyEntryMode,
  type WsrxRefreshSource,
} from '@Utils/WsrxTunnel'
import { useConfig } from '@Hooks/useConfig'
import api, { ClientFlagContext, ContainerPortMappingType } from '@Api'
import classes from '@Styles/InstanceEntry.module.css'
import misc from '@Styles/Misc.module.css'

dayjs.extend(duration)

const CAPABILITY_REFRESH_SAFETY_MS = 5 * 60 * 1000

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
  const { wsrx, wsrxState, wsrxInstances, wsrxOptions, doWsrxConnect, watchPendingTunnel } = useWsrx()

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
  const [capabilityExpiresAt, setCapabilityExpiresAt] = useState<number | null>(null)
  const [capabilityRefreshAt, setCapabilityRefreshAt] = useState<number | null>(null)
  const [tunnelRequestComplete, setTunnelRequestComplete] = useState(false)
  const [tunnelRequestFailed, setTunnelRequestFailed] = useState(false)
  const [tunnelCheckExpired, setTunnelCheckExpired] = useState(false)
  const [tunnelRetrying, setTunnelRetrying] = useState(false)
  const [readinessGeneration, setReadinessGeneration] = useState(0)
  const capabilityRequestSequence = useRef(0)
  const capabilityRefreshInFlight = useRef(false)
  const currentCapabilityExpiresAt = useRef<number | null>(null)

  const requestProxyCapability = useCallback(async () => {
    if (!isPlatformProxy || !instanceEntry) return null

    const response = isPreview
      ? await api.proxy.proxyIssueNoInstanceCapability(instanceEntry)
      : await api.proxy.proxyIssueInstanceCapability(instanceEntry)
    return {
      remoteEntry: getProxyEntry(instanceEntry, isPreview, response.data.token),
      expiresAt: response.data.expiresAt,
    }
  }, [instanceEntry, isPlatformProxy, isPreview])

  useEffect(() => {
    currentCapabilityExpiresAt.current = null
    setWsrxRemoteEntry('')
    setCapabilityExpiresAt(null)
    setCapabilityRefreshAt(null)
    setTunnelRequestComplete(false)
    setTunnelRequestFailed(false)
    setTunnelCheckExpired(false)
    if (!isPlatformProxy || !instanceEntry) return

    let active = true
    const requestSequence = ++capabilityRequestSequence.current
    const requestCapability = async () => {
      try {
        const capability = await requestProxyCapability()
        if (active && capability && isLatestWsrxCapabilityRequest(requestSequence, capabilityRequestSequence.current)) {
          currentCapabilityExpiresAt.current = capability.expiresAt
          setWsrxRemoteEntry(capability.remoteEntry)
          setCapabilityExpiresAt(capability.expiresAt)
          setCapabilityRefreshAt(capability.expiresAt - CAPABILITY_REFRESH_SAFETY_MS)
        }
      } catch (err) {
        if (active && isLatestWsrxCapabilityRequest(requestSequence, capabilityRequestSequence.current)) {
          setTunnelRequestFailed(true)
          HandleWsrxError(err, t)
        }
      }
    }

    requestCapability()
    return () => {
      active = false
    }
  }, [instanceEntry, isPlatformProxy, requestProxyCapability, t])

  const localTraffic = wsrxInstances.find((traffic) => traffic.remote === wsrxRemoteEntry)

  useEffect(() => {
    if (tunnelRetrying) return

    const action = getLocalWsrxTunnelAction({
      mode: proxyEntryMode,
      state: wsrxState,
      remoteEntry: wsrxRemoteEntry,
      localEntry: localTraffic?.local,
      allowLan: wsrxOptions.allowLan,
    })
    if (action === 'idle') return
    if (action === 'reuse') {
      setTunnelRequestComplete(true)
      setTunnelRequestFailed(false)
      return
    }

    const localAddr = wsrxOptions.allowLan ? '0.0.0.0:0' : '127.0.0.1:0'
    let active = true
    setTunnelRequestComplete(false)
    setTunnelRequestFailed(false)

    const requestProxy = async () => {
      try {
        if (action === 'rebind' && localTraffic?.local) {
          await wsrx.delete(localTraffic.local)
          return
        }

        await wsrx.add({
          label,
          remote: wsrxRemoteEntry,
          local: localAddr,
        })
        if (active) setTunnelRequestComplete(true)
      } catch (err) {
        if (active) {
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
  }, [
    wsrx,
    wsrxRemoteEntry,
    wsrxState,
    label,
    localTraffic?.local,
    proxyEntryMode,
    t,
    tunnelRetrying,
    wsrxOptions.allowLan,
  ])

  useEffect(() => {
    setTunnelCheckExpired(false)
    if (!localTraffic || localTraffic.latency !== -1) return

    // One provider-owned completion scheduler serves every pending entry and
    // falls back to the daemon library's ordinary sync after this bounded window.
    return watchPendingTunnel(wsrxRemoteEntry, () => setTunnelCheckExpired(true))
  }, [localTraffic?.latency, localTraffic?.local, readinessGeneration, watchPendingTunnel, wsrxRemoteEntry])

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
    async (source: WsrxRefreshSource) => {
      if (!isPlatformProxy || capabilityRefreshInFlight.current) return

      capabilityRefreshInFlight.current = true
      setTunnelRetrying(true)
      const requestSequence = ++capabilityRequestSequence.current
      try {
        const capability = await requestProxyCapability()
        if (!capability || !isLatestWsrxCapabilityRequest(requestSequence, capabilityRequestSequence.current)) return

        if (wsrxState === WsrxState.Usable && localTraffic?.local) {
          await wsrx.delete(localTraffic.local)
        }
        if (!isLatestWsrxCapabilityRequest(requestSequence, capabilityRequestSequence.current)) return

        currentCapabilityExpiresAt.current = capability.expiresAt
        setWsrxRemoteEntry(capability.remoteEntry)
        setCapabilityExpiresAt(capability.expiresAt)
        setCapabilityRefreshAt(capability.expiresAt - CAPABILITY_REFRESH_SAFETY_MS)
        setTunnelRequestComplete(false)
        setTunnelRequestFailed(false)
        setTunnelCheckExpired(false)
        setReadinessGeneration((generation) => generation + 1)
        if (shouldConnectLocalWsrx({ mode: proxyEntryMode, source, state: wsrxState })) doWsrxConnect()
      } catch (err) {
        if (isLatestWsrxCapabilityRequest(requestSequence, capabilityRequestSequence.current)) {
          if (capabilityExpiresAt !== null) {
            setCapabilityRefreshAt(getWsrxCapabilityRetryAt(getServerNowMilliseconds(), capabilityExpiresAt))
          }
          HandleWsrxError(err, t)
        }
      } finally {
        capabilityRefreshInFlight.current = false
        setTunnelRetrying(false)
      }
    },
    [
      doWsrxConnect,
      isPlatformProxy,
      localTraffic?.local,
      proxyEntryMode,
      requestProxyCapability,
      t,
      wsrx,
      wsrxState,
      capabilityExpiresAt,
    ]
  )

  useServerClockTimeout(
    () => void onRefreshProxyEntry('automatic'),
    isPlatformProxy ? capabilityRefreshAt : null,
    0,
    1000
  )

  const invalidateExpiredProxyCapability = useCallback(() => {
    if (
      capabilityExpiresAt === null ||
      !shouldInvalidateWsrxCapability(
        getServerNowMilliseconds(),
        capabilityExpiresAt,
        currentCapabilityExpiresAt.current
      )
    )
      return

    currentCapabilityExpiresAt.current = null
    setWsrxRemoteEntry('')
    setCapabilityExpiresAt(null)
    setCapabilityRefreshAt(null)
    setTunnelRequestComplete(false)
    setTunnelRequestFailed(true)
    setTunnelCheckExpired(false)
    if (wsrxState === WsrxState.Usable && localTraffic?.local) {
      void wsrx.delete(localTraffic.local).catch(() => undefined)
    }
  }, [capabilityExpiresAt, localTraffic?.local, wsrx, wsrxState])

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
