import {
  ActionIcon,
  Card,
  Group,
  ScrollArea,
  Stack,
  Switch,
  Text,
  TextInput,
  useMantineColorScheme,
  useMantineTheme,
} from '@mantine/core'
import { useLocalStorage } from '@mantine/hooks'
import { useDebouncedValue } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import {
  mdiAccountGroupOutline,
  mdiAccountOutline,
  mdiArrowLeftBold,
  mdiArrowRightBold,
  mdiCheck,
  mdiClose,
  mdiDownload,
  mdiExclamationThick,
  mdiFlag,
  mdiLightningBolt,
  mdiMagnify,
  mdiReplay,
  mdiToggleSwitchOffOutline,
  mdiToggleSwitchOutline,
  mdiEyeOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import * as signalR from '@microsoft/signalr'
import cx from 'clsx'
import dayjs from 'dayjs'
import { FC, useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { ScrollingText } from '@Components/ScrollingText'
import { WithGameMonitor } from '@Components/WithGameMonitor'
import { SwitchLabel } from '@Components/admin/SwitchLabel'
import { handleAxiosError } from '@Utils/ApiHelper'
import { useLanguage } from '@Utils/I18n'
import { gameEventMonitorIdentity, unreconciledMonitorRows } from '@Utils/MonitorFeed'
import { useIsMobile } from '@Utils/ThemeOverride'
import { useGame, useGameStatus, useRevalidateWhenPollingStops } from '@Hooks/useGame'
import api, { EventType, GameEvent } from '@Api'
import tableClasses from '@Styles/Table.module.css'
import { formatGameEvent } from '../eventFormat'

const ITEM_COUNT_PER_PAGE = 30

const EventTypeIconMap = (size: number) => {
  const theme = useMantineTheme()
  const { colorScheme } = useMantineColorScheme()

  return useMemo(() => {
    const colorIdx = colorScheme === 'dark' ? 5 : 6
    return new Map([
      [EventType.FlagSubmit, { path: mdiFlag, size, color: theme.colors.cyan[colorIdx] }],
      [EventType.ContainerStart, { path: mdiToggleSwitchOutline, size, color: theme.colors.green[colorIdx] }],
      [EventType.ContainerDestroy, { path: mdiToggleSwitchOffOutline, size, color: theme.colors.red[colorIdx] }],
      [EventType.CheatDetected, { path: mdiExclamationThick, size, color: theme.colors.orange[colorIdx] }],
      [EventType.Download, { path: mdiDownload, size, color: theme.colors.cyan[colorIdx] }],
      [EventType.ChallengeOpened, { path: mdiEyeOutline, size, color: theme.colors.violet[colorIdx] }],
      [EventType.Normal, { path: mdiLightningBolt, size, color: theme.colors.light[colorIdx] }],
    ])
  }, [size, colorScheme, theme.colors])
}

interface IconBadgeProps {
  path: string
  content?: string
}

const IconBadge: FC<IconBadgeProps> = ({ path, content }) => {
  return (
    <Group gap={3} wrap="nowrap">
      <Icon path={path} size={0.75} color="var(--mantine-color-dimmed)" />
      <ScrollingText text={content ?? ''} size="sm" fw={500} c="dimmed" maw={180} />
    </Group>
  )
}

const Events: FC = () => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')

  const [hideContainerEvents, setHideContainerEvents] = useLocalStorage({
    key: 'hide-container-events',
    defaultValue: false,
    getInitialValueInEffect: false,
  })

  const { locale } = useLanguage()

  const [activePage, setPage] = useState(1)
  const [search, setSearch] = useState('')
  const [debouncedSearch] = useDebouncedValue(search, 500)

  const [, update] = useState(new Date())
  const newEvents = useRef<GameEvent[]>([])
  const [events, setEvents] = useState<GameEvent[]>()

  const { game } = useGame(numId)
  const { finished } = useGameStatus(game)
  const monitorConnectionActive = Boolean(game?.end) && !finished
  const isNarrow = useIsMobile(480)

  const iconMap = EventTypeIconMap(1.15)
  const { t } = useTranslation()
  const viewport = useRef<HTMLDivElement>(null)

  useEffect(() => {
    viewport.current?.scrollTo({ top: 0, behavior: 'smooth' })
  }, [activePage, viewport])

  const fetchEvents = useCallback(async () => {
    try {
      const res = await api.game.gameEvents(numId, {
        hideContainer: hideContainerEvents,
        count: ITEM_COUNT_PER_PAGE,
        skip: (activePage - 1) * ITEM_COUNT_PER_PAGE,
        search: debouncedSearch || undefined,
      })
      setEvents(res.data)
    } catch (err) {
      showNotification({
        color: 'red',
        title: t('game.notification.fetch_failed.event'),
        message: await handleAxiosError(err),
        icon: <Icon path={mdiClose} size={1} />,
      })
    }
  }, [activePage, hideContainerEvents, debouncedSearch, numId, t])

  useEffect(() => {
    void fetchEvents()

    if (activePage === 1) {
      newEvents.current = []
    }
  }, [activePage, fetchEvents])

  useEffect(() => {
    if (monitorConnectionActive) {
      const connection = new signalR.HubConnectionBuilder()
        .withUrl(`/hub/monitor?game=${numId}`)
        .withHubProtocol(new signalR.JsonHubProtocol())
        .withAutomaticReconnect()
        .configureLogging(signalR.LogLevel.None)
        .build()

      connection.serverTimeoutInMilliseconds = 60 * 1000 * 60 * 2

      connection.on('ReceivedGameEvent', (message: GameEvent) => {
        console.log(message)
        newEvents.current = [message, ...newEvents.current]
        update(new Date(message.time!))
      })

      const startConnection = async () => {
        try {
          await connection.start()
          showNotification({
            color: 'teal',
            message: t('game.notification.connected.event'),
            icon: <Icon path={mdiCheck} size={1} />,
          })
        } catch (err) {
          console.error(err)
        }
      }

      startConnection()

      return () => {
        connection.stop().catch((err) => {
          console.error(err)
        })
      }
    }
  }, [monitorConnectionActive, numId, t])

  // This effect is intentionally declared after hub ownership. React tears
  // down the live connection first, then publishes one authoritative REST
  // snapshot for a mounted active -> stopped lifecycle transition.
  useRevalidateWhenPollingStops(monitorConnectionActive, fetchEvents)

  const filteredEvents = newEvents.current.filter(
    (e) => !hideContainerEvents || (e.type !== EventType.ContainerStart && e.type !== EventType.ContainerDestroy)
  )
  const bufferedEvents =
    activePage === 1 ? unreconciledMonitorRows(filteredEvents, events ?? [], gameEventMonitorIdentity) : []
  const visibleEvents = [...bufferedEvents, ...(events ?? [])]

  return (
    <WithGameMonitor isLoading={!events}>
      <Group justify="space-between" w="100%">
        <Switch
          label={SwitchLabel(
            t('game.content.hide_container_events.label'),
            t('game.content.hide_container_events.description')
          )}
          checked={hideContainerEvents}
          onChange={(e) => setHideContainerEvents(e.currentTarget.checked)}
        />
        <TextInput
          label={t('game.label.events.search', 'Search events')}
          placeholder={t('game.label.events.search_placeholder', 'Search events...')}
          leftSection={<Icon path={mdiMagnify} size={0.8} />}
          value={search}
          onChange={(e) => {
            setSearch(e.currentTarget.value)
            setPage(1)
          }}
          style={{ width: 250 }}
        />
        <Group justify="right">
          <ActionIcon
            size="lg"
            aria-label={t('common.pagination.first', 'First page')}
            disabled={activePage <= 1}
            onClick={() => setPage(1)}
          >
            <Icon path={mdiReplay} size={1} />
          </ActionIcon>
          <ActionIcon
            size="lg"
            aria-label={t('common.pagination.previous', 'Previous page')}
            disabled={activePage <= 1}
            onClick={() => setPage(activePage - 1)}
          >
            <Icon path={mdiArrowLeftBold} size={1} />
          </ActionIcon>
          <ActionIcon
            size="lg"
            aria-label={t('common.pagination.next', 'Next page')}
            disabled={events && events.length < ITEM_COUNT_PER_PAGE}
            onClick={() => setPage(activePage + 1)}
          >
            <Icon path={mdiArrowRightBold} size={1} />
          </ActionIcon>
        </Group>
      </Group>
      <ScrollArea
        viewportRef={viewport}
        offsetScrollbars
        h="calc(100vh - 160px)"
        viewportProps={{
          role: 'region',
          tabIndex: 0,
          'aria-label': t('game.label.events.stream', 'Event stream'),
        }}
      >
        <Stack gap="xs" pr={10} w="100%">
          {visibleEvents.map((event, i) => (
            <Card
              shadow="sm"
              p="xs"
              key={`${event.time}@${i}`}
              className={cx({ [tableClasses.fade]: i === 0 && bufferedEvents.length > 0 })}
            >
              <Group wrap="nowrap" align="flex-start" justify="right" gap="sm" w="100%">
                <Icon {...iconMap.get(event.type)!} />
                <Stack gap={2} w="100%" style={{ minWidth: 0 }}>
                  <ScrollingText text={formatGameEvent(t, event)} size="md" fw={500} maw={800} />
                  {isNarrow ? (
                    <Stack gap={4}>
                      <Group gap="xs" wrap="wrap" style={{ minWidth: 0 }}>
                        <IconBadge path={mdiAccountOutline} content={event.user} />
                        <IconBadge path={mdiAccountGroupOutline} content={event.team} />
                      </Group>
                      <Text size="xs" fw={500} c="dimmed" style={{ alignSelf: 'flex-end' }}>
                        {dayjs(event.time).locale(locale).format('SL LTS')}
                      </Text>
                    </Stack>
                  ) : (
                    <Group wrap="nowrap" justify="space-between">
                      <Group gap="sm" wrap="nowrap" style={{ minWidth: 0 }}>
                        <IconBadge path={mdiAccountOutline} content={event.user} />
                        <IconBadge path={mdiAccountGroupOutline} content={event.team} />
                      </Group>
                      <Text size="xs" fw={500} c="dimmed">
                        {dayjs(event.time).locale(locale).format('SL LTS')}
                      </Text>
                    </Group>
                  )}
                </Stack>
              </Group>
            </Card>
          ))}
        </Stack>
      </ScrollArea>
    </WithGameMonitor>
  )
}

export default Events
