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
import { LatestRequest } from '@Utils/LatestRequest'
import {
  currentMonitorBufferRows,
  currentMonitorSnapshotRows,
  gameEventMonitorIdentity,
  mergeGameEventBuffer,
  monitorEventPushIsCurrent,
  monitorSnapshotIsCurrent,
  rebaseGameEventBuffer,
  type ScopedMonitorSnapshot,
  unreconciledMonitorRows,
} from '@Utils/MonitorFeed'
import { OPERATOR_FALLBACK_POLL_MS } from '@Utils/SignalRRecovery'
import { useIsMobile } from '@Utils/ThemeOverride'
import { useViewerIdentity } from '@Utils/ViewerIdentity'
import { useGame, useGameStatus, useRevalidateWhenPollingStops } from '@Hooks/useGame'
import { useRecoveringHub } from '@Hooks/useRecoveringHub'
import api, { EventType, GameEvent } from '@Api'
import tableClasses from '@Styles/Table.module.css'
import { formatGameEvent } from '../eventFormat'

const ITEM_COUNT_PER_PAGE = 30
const BACKFILL_PAGE_SIZE = 100
const MAX_BACKFILL_PAGES = 10
const MAX_BUFFERED_EVENTS = 500

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
  const { scope: viewerScope } = useViewerIdentity()
  const feedScope = JSON.stringify([viewerScope, numId])
  const snapshotScope = JSON.stringify([feedScope, activePage, hideContainerEvents, debouncedSearch])

  const [, update] = useState(0)
  const newEvents = useRef<GameEvent[]>([])
  const bufferedFeedScope = useRef(feedScope)
  const eventCursor = useRef(0)
  const cursorInitialized = useRef(false)
  const activeFeedScope = useRef(feedScope)
  const activeSnapshotScope = useRef(snapshotScope)
  const latestSnapshotRequest = useRef(0)
  const [eventSnapshot, setEventSnapshot] = useState<ScopedMonitorSnapshot<GameEvent>>()
  const events = currentMonitorSnapshotRows(snapshotScope, eventSnapshot)
  const eventRequest = useRef(new LatestRequest())
  const recoveryRequest = useRef(new LatestRequest())

  const { game } = useGame(numId)
  const { finished, status: gameStatus } = useGameStatus(game)
  const monitorConnectionActive = Boolean(game?.end) && !finished
  const isNarrow = useIsMobile(480)

  const iconMap = EventTypeIconMap(1.15)
  const { t } = useTranslation()
  const viewport = useRef<HTMLDivElement>(null)

  useEffect(() => {
    activeFeedScope.current = feedScope
    activeSnapshotScope.current = snapshotScope
  }, [feedScope, snapshotScope])

  useEffect(() => {
    viewport.current?.scrollTo({ top: 0, behavior: 'smooth' })
  }, [activePage, viewport])

  const loadSnapshot = useCallback(async () => {
    const requestId = ++latestSnapshotRequest.current
    const res = await eventRequest.current.run((signal) =>
      api.game.gameEventPage(
        numId,
        {
          hideContainer: hideContainerEvents,
          count: ITEM_COUNT_PER_PAGE,
          skip: (activePage - 1) * ITEM_COUNT_PER_PAGE,
          search: debouncedSearch || undefined,
        },
        { signal }
      )
    )
    if (
      !res ||
      !monitorSnapshotIsCurrent(activeSnapshotScope.current, snapshotScope, latestSnapshotRequest.current, requestId)
    ) {
      return
    }
    setEventSnapshot({ scope: snapshotScope, rows: res.data })
    return res.data
  }, [activePage, hideContainerEvents, debouncedSearch, numId, snapshotScope])

  const fetchEvents = useCallback(async () => {
    try {
      return await loadSnapshot()
    } catch (err) {
      showNotification({
        color: 'red',
        title: t('game.notification.fetch_failed.event'),
        message: await handleAxiosError(err),
        icon: <Icon path={mdiClose} size={1} />,
      })
    }
  }, [loadSnapshot, t])

  const mergeIncomingEvents = useCallback((incoming: readonly GameEvent[]) => {
    if (incoming.length === 0) return
    newEvents.current = mergeGameEventBuffer(incoming, newEvents.current, MAX_BUFFERED_EVENTS)
    update((version) => version + 1)
  }, [])

  const rebaseAtCheckpoint = useCallback((checkpoint: number) => {
    newEvents.current = rebaseGameEventBuffer(newEvents.current, checkpoint)
    update((version) => version + 1)
  }, [])

  useEffect(() => {
    eventCursor.current = 0
    cursorInitialized.current = false
    newEvents.current = []
    bufferedFeedScope.current = feedScope
    setEventSnapshot(undefined)
    update((version) => version + 1)
    return () => {
      eventRequest.current.cancel()
      recoveryRequest.current.cancel()
    }
  }, [feedScope])

  useEffect(() => {
    void fetchEvents()

    if (activePage === 1) {
      newEvents.current = []
    }

    return () => eventRequest.current.cancel()
  }, [activePage, fetchEvents])

  const reconcileEvents = useCallback(
    () =>
      recoveryRequest.current.run(async (signal) => {
        const requestedFeedScope = feedScope
        const isCurrent = () => activeFeedScope.current === requestedFeedScope && !signal.aborted

        // Establish the initial durable prefix only after the listener is live.
        // The snapshot represents older matching rows; pushes newer than this
        // checkpoint remain buffered and cannot fall into the handshake gap.
        if (!cursorInitialized.current) {
          const checkpoint = await api.game.gameEventBackfill(numId, {}, { signal })
          if (!isCurrent()) return
          const snapshot = await loadSnapshot()
          if (!isCurrent() || snapshot === undefined) return
          rebaseAtCheckpoint(checkpoint.data.nextCursor)
          eventCursor.current = checkpoint.data.nextCursor
          cursorInitialized.current = true
          return
        }

        let cursor = eventCursor.current
        for (let page = 0; page < MAX_BACKFILL_PAGES && isCurrent(); page += 1) {
          const response = await api.game.gameEventBackfill(
            numId,
            { after: cursor, limit: BACKFILL_PAGE_SIZE },
            { signal }
          )
          if (!isCurrent()) return
          if (response.data.nextCursor < cursor || (response.data.nextCursor === cursor && response.data.hasMore)) {
            throw new Error('Monitor event backfill cursor did not advance')
          }
          mergeIncomingEvents(response.data.events)
          cursor = response.data.nextCursor
          eventCursor.current = cursor
          if (!response.data.hasMore) {
            await loadSnapshot()
            return
          }
        }

        if (!isCurrent()) return
        // Cap recovery at ten pages. For a larger outage, fence the durable tail,
        // take one authoritative visible page, and discard only buffered rows
        // represented by that checkpoint rather than issuing an unbounded loop.
        const checkpoint = await api.game.gameEventBackfill(numId, {}, { signal })
        if (!isCurrent()) return
        const snapshot = await loadSnapshot()
        if (!isCurrent() || snapshot === undefined) return
        rebaseAtCheckpoint(checkpoint.data.nextCursor)
        eventCursor.current = Math.max(eventCursor.current, checkpoint.data.nextCursor)
      }),
    [feedScope, loadSnapshot, mergeIncomingEvents, numId, rebaseAtCheckpoint]
  )

  const { waitForStop: waitForMonitorHubStop } = useRecoveringHub({
    active: monitorConnectionActive,
    url: `/hub/monitor?game=${numId}`,
    ownerKey: gameStatus,
    handlers: {
      ReceivedGameEvent: (raw) => {
        const message = raw as GameEvent
        if (
          !monitorEventPushIsCurrent(
            activeFeedScope.current,
            feedScope,
            false,
            cursorInitialized.current,
            eventCursor.current,
            message.cursor
          )
        )
          return
        mergeIncomingEvents([message])
      },
    },
    revalidate: reconcileEvents,
    pollingIntervalMs: OPERATOR_FALLBACK_POLL_MS,
    onConnected: () =>
      showNotification({
        color: 'teal',
        message: t('game.notification.connected.event'),
        icon: <Icon path={mdiCheck} size={1} />,
      }),
  })

  // The final snapshot starts only after stop() has removed the listener. A
  // pre-close operation that commits during shutdown is then represented by
  // either its push or this one authoritative backfill, never by neither.
  useRevalidateWhenPollingStops(monitorConnectionActive, reconcileEvents, waitForMonitorHubStop)

  const normalizedSearch = debouncedSearch.trim().toLocaleLowerCase(locale)
  const currentBufferedEvents = currentMonitorBufferRows(feedScope, bufferedFeedScope.current, newEvents.current)
  const filteredEvents = currentBufferedEvents.filter((event) => {
    if (hideContainerEvents && (event.type === EventType.ContainerStart || event.type === EventType.ContainerDestroy)) {
      return false
    }
    if (!normalizedSearch) return true
    return [event.user, event.team, ...event.values]
      .filter((value): value is string => typeof value === 'string')
      .some((value) => value.toLocaleLowerCase(locale).includes(normalizedSearch))
  })
  const bufferedEvents =
    activePage === 1 ? unreconciledMonitorRows(filteredEvents, events ?? [], gameEventMonitorIdentity) : []
  const visibleEvents =
    activePage === 1 ? mergeGameEventBuffer(bufferedEvents, events ?? [], MAX_BUFFERED_EVENTS) : (events ?? [])

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
              key={event.id}
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
