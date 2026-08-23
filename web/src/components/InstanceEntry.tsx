import { ActionIcon, Anchor, Button, Divider, Group, Stack, Text, TextInput, Tooltip } from '@mantine/core'
import { useClipboard } from '@mantine/hooks'
import { useDebouncedCallback, useDebouncedState } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiContentCopy, mdiExclamation, mdiOpenInNew, mdiRefresh, mdiServerNetwork } from '@mdi/js'
import { Icon } from '@mdi/react'
import { WsrxState } from '@xdsec/wsrx'
import dayjs from 'dayjs'
import duration from 'dayjs/plugin/duration'
import { FC, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { HandleWsrxError, useWsrx } from '@Components/WsrxProvider'
import { getProxyUrl as getProxyEntry } from '@Utils/Shared'
import { getWsrxTunnelPhase } from '@Utils/WsrxTunnel'
import { useConfig } from '@Hooks/useConfig'
import { useTicker } from '@Hooks/useTicker'
import api, { ClientFlagContext, ContainerPortMappingType } from '@Api'
import classes from '@Styles/InstanceEntry.module.css'
import misc from '@Styles/Misc.module.css'

dayjs.extend(duration)

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
  onExtend?: () => void
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
  const now = useTicker()
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

  const [canExtend, setCanExtend] = useDebouncedState(false, 500)

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
    const countdown = dayjs.duration(dayjs(context.closeTime ?? 0).diff(dayjs()))
    setCanExtend(countdown.asMinutes() < (config.renewalWindow ?? 10))
  }, [context, config.renewalWindow])

  const onExtend = async () => {
    if (!canExtend || !props.onExtend) return

    try {
      await Promise.resolve(props.onExtend())

      showNotification({
        color: 'teal',
        title: t('challenge.notification.instance.extend.success.title'),
        message: t('challenge.notification.instance.extend.success.message'),
        icon: <Icon path={mdiCheck} size={1} />,
      })

      setCanExtend(false)
    } catch (err) {
      showNotification({
        color: 'red',
        title: t('challenge.notification.instance.extend.note.title'),
        message: (err as Error)?.message ?? t('common.error.unknown', 'An unknown error occurred'),
        icon: <Icon path={mdiExclamation} size={1} />,
      })
    }
  }

  // Platform-proxied instances only become usable after the local daemon has
  // verified its tunnel. Never present the raw WebSocket capability as a
  // netcat address.
  const isWsrxUsable = isPlatformProxy && wsrxState === WsrxState.Usable
  const [wsrxRemoteEntry, setWsrxRemoteEntry] = useState('')
  const [capabilityAttempt, setCapabilityAttempt] = useState(0)
  const [tunnelRequestComplete, setTunnelRequestComplete] = useState(false)
  const [tunnelRequestFailed, setTunnelRequestFailed] = useState(false)
  const [tunnelCheckExpired, setTunnelCheckExpired] = useState(false)
  const [tunnelRetrying, setTunnelRetrying] = useState(false)

  useEffect(() => {
    setWsrxRemoteEntry('')
    setTunnelRequestComplete(false)
    setTunnelRequestFailed(false)
    setTunnelCheckExpired(false)
    if (!isWsrxUsable || !instanceEntry) return

    let active = true
    const requestCapability = async () => {
      try {
        const response = isPreview
          ? await api.proxy.proxyIssueNoInstanceCapability(instanceEntry)
          : await api.proxy.proxyIssueInstanceCapability(instanceEntry)
        if (active) setWsrxRemoteEntry(getProxyEntry(instanceEntry, isPreview, response.data.token))
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
  }, [capabilityAttempt, instanceEntry, isPreview, isWsrxUsable, t])

  const localTraffic = wsrxInstances.find((traffic) => traffic.remote === wsrxRemoteEntry)

  useEffect(() => {
    if (!wsrxRemoteEntry || !isWsrxUsable) return

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
  }, [wsrx, wsrxRemoteEntry, isWsrxUsable, label, t, wsrxOptions.allowLan])

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

  const entry = isPlatformProxy ? (phase === 'ready' ? (localTraffic?.local ?? '') : '') : instanceEntry
  const canUseEntry = !!entry

  const onRetryTunnel = async () => {
    if (!isPlatformProxy || tunnelRetrying) return

    setTunnelRetrying(true)
    setWsrxRemoteEntry('')
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

    if (wsrxState !== WsrxState.Usable) doWsrxConnect()
    setCapabilityAttempt((attempt) => attempt + 1)
    setTunnelRetrying(false)
  }

  const tunnelStatusColor = phase === 'ready' ? 'green' : phase === 'unhealthy' ? 'red' : 'orange'

  const onCopyEntry = () => {
    if (!canUseEntry) return
    clipBoard.copy(entry)

    showNotification({
      color: 'teal',
      message: t('challenge.notification.instance.copied.entry'),
      icon: <Icon path={mdiCheck} size={1} />,
    })
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
        label={
          <Text size="sm" fw="bold">
            {t('challenge.content.instance.entry.label')}
          </Text>
        }
        description={
          isPlatformProxy && (
            <Stack gap={2}>
              <Text span size="sm">
                {t('wsrx.tunnel.description')}&nbsp;
                <Anchor href="https://github.com/XDSEC/WebSocketReflectorX/releases" target="_blank" rel="noreferrer">
                  {t('challenge.content.instance.entry.description.anchor')}
                </Anchor>
              </Text>
              <Text size="xs" c={tunnelStatusColor} role="status" aria-live="polite">
                {t(`wsrx.tunnel.${phase}`)}
              </Text>
            </Stack>
          )
        }
        leftSection={
          <Icon
            path={mdiServerNetwork}
            size={1}
            data-proxied={phase === 'ready' || undefined}
            className={classes.icon}
          />
        }
        value={entry}
        placeholder={isPlatformProxy ? t('wsrx.tunnel.placeholder') : undefined}
        readOnly
        classNames={{ input: misc.ffmono }}
        rightSection={
          <Group gap={2} wrap="nowrap">
            <Divider orientation="vertical" pr={4} />
            {isPlatformProxy && (
              <Tooltip label={t('wsrx.button.retry_tunnel')} withArrow>
                <ActionIcon aria-label={t('wsrx.button.retry_tunnel')} onClick={onRetryTunnel} loading={tunnelRetrying}>
                  <Icon path={mdiRefresh} size={1} />
                </ActionIcon>
              </Tooltip>
            )}
            <Tooltip label={t('common.button.copy')} withArrow>
              <ActionIcon aria-label={t('common.button.copy')} onClick={onCopyEntry} disabled={!canUseEntry}>
                <Icon path={mdiContentCopy} size={1} />
              </ActionIcon>
            </Tooltip>
            <Tooltip label={t('challenge.content.instance.open.web')} withArrow>
              <ActionIcon
                aria-label={t('challenge.content.instance.open.web')}
                disabled={!canUseEntry}
                component="a"
                href={
                  canUseEntry
                    ? `http://${isPlatformProxy && wsrxOptions.allowLan ? entry.replace('0.0.0.0', '127.0.0.1') : entry}`
                    : undefined
                }
                target={canUseEntry ? '_blank' : undefined}
                rel="noreferrer"
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
