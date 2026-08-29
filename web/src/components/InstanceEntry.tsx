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
import { FC, useCallback, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { HandleWsrxError, useWsrx } from '@Components/WsrxProvider'
import { isInstanceExtensionWindowOpen, runInstanceExtension } from '@Utils/InstanceLifecycle'
import { getServerNowMilliseconds, useServerClockOffset, useServerClockTimeout, useServerNow } from '@Utils/ServerClock'
import { getProxyUrl as getProxyEntry } from '@Utils/Shared'
import {
  DEFAULT_PROXY_ENTRY_MODE,
  getWsrxTunnelPhase,
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
  const { wsrx, wsrxState, wsrxInstances, wsrxOptions, doWsrxConnect } = useWsrx()

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
  const isWsrxUsable = isPlatformProxy && wsrxState === WsrxState.Usable
  const [proxyEntryMode, setProxyEntryMode] = useState<ProxyEntryMode>(DEFAULT_PROXY_ENTRY_MODE)
  const [wsrxRemoteEntry, setWsrxRemoteEntry] = useState('')
  const [capabilityExpiresAt, setCapabilityExpiresAt] = useState<number | null>(null)
  const [capabilityAttempt, setCapabilityAttempt] = useState(0)
  const [tunnelRequestComplete, setTunnelRequestComplete] = useState(false)
  const [tunnelRequestFailed, setTunnelRequestFailed] = useState(false)
  const [tunnelCheckExpired, setTunnelCheckExpired] = useState(false)
  const [tunnelRetrying, setTunnelRetrying] = useState(false)

  useEffect(() => {
    setWsrxRemoteEntry('')
    setCapabilityExpiresAt(null)
    setTunnelRequestComplete(false)
    setTunnelRequestFailed(false)
    setTunnelCheckExpired(false)
    if (!isPlatformProxy || !instanceEntry) return

    let active = true
    const requestCapability = async () => {
      try {
        const response = isPreview
          ? await api.proxy.proxyIssueNoInstanceCapability(instanceEntry)
          : await api.proxy.proxyIssueInstanceCapability(instanceEntry)
        if (active) {
          setWsrxRemoteEntry(getProxyEntry(instanceEntry, isPreview, response.data.token))
          setCapabilityExpiresAt(response.data.expiresAt)
        }
      } catch (err) {
        if (active) {
          setTunnelRequestFailed(true)
          HandleWsrxError(err, t)
        }
      }
    }

    requestCapability()
    return () => {
      active = false
    }
  }, [capabilityAttempt, instanceEntry, isPlatformProxy, isPreview, t])

  const localTraffic = wsrxInstances.find((traffic) => traffic.remote === wsrxRemoteEntry)

  useEffect(() => {
    if (proxyEntryMode !== 'wsrx' || !wsrxRemoteEntry || !isWsrxUsable) return

    const localAddr = wsrxOptions.allowLan ? '0.0.0.0:0' : '127.0.0.1:0'
    let active = true

    const requestProxy = async () => {
      try {
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
  }, [wsrx, wsrxRemoteEntry, isWsrxUsable, label, proxyEntryMode, t, wsrxOptions.allowLan])

  useEffect(() => {
    setTunnelCheckExpired(false)
    if (!localTraffic || localTraffic.latency !== -1) return

    // The desktop daemon calculates latency after returning from POST /pool.
    // Pull the result promptly instead of waiting for the client's 15-second
    // background refresh before deciding whether the local address is usable.
    const refresh = window.setInterval(() => void wsrx.sync().catch(() => undefined), 1500)
    const timeout = window.setTimeout(() => setTunnelCheckExpired(true), 8000)
    return () => {
      window.clearInterval(refresh)
      window.clearTimeout(timeout)
    }
  }, [localTraffic?.latency, localTraffic?.local, wsrx, wsrxRemoteEntry])

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
      if (!isPlatformProxy || tunnelRetrying) return

      setTunnelRetrying(true)
      setWsrxRemoteEntry('')
      setCapabilityExpiresAt(null)
      setTunnelRequestComplete(false)
      setTunnelRequestFailed(false)
      setTunnelCheckExpired(false)

      if (wsrxState === WsrxState.Usable && localTraffic?.local) {
        try {
          await wsrx.delete(localTraffic.local)
        } catch (err) {
          HandleWsrxError(err, t)
        }
      }

      if (shouldConnectLocalWsrx({ mode: proxyEntryMode, source, state: wsrxState })) doWsrxConnect()
      setCapabilityAttempt((attempt) => attempt + 1)
      setTunnelRetrying(false)
    },
    [doWsrxConnect, isPlatformProxy, localTraffic?.local, proxyEntryMode, t, tunnelRetrying, wsrx, wsrxState]
  )

  useServerClockTimeout(
    () => void onRefreshProxyEntry('automatic'),
    isPlatformProxy ? capabilityExpiresAt : null,
    CAPABILITY_REFRESH_SAFETY_MS,
    1000
  )

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
                value={proxyEntryMode}
                onChange={(value) => {
                  const mode = value as ProxyEntryMode
                  setProxyEntryMode(mode)
                  if (shouldConnectLocalWsrx({ mode, source: 'player', state: wsrxState })) doWsrxConnect()
                }}
                data={[
                  {
                    label: <span data-guide="wsrx-local-mode">{t('wsrx.mode.local')}</span>,
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
