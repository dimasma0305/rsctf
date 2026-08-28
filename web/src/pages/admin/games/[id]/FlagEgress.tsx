import {
  ActionIcon,
  Center,
  Group,
  Loader,
  Paper,
  ScrollArea,
  Stack,
  Table,
  Text,
  TextInput,
  ThemeIcon,
  Tooltip,
} from '@mantine/core'
import { useDebouncedValue, useReducedMotion } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import { mdiArrowLeftBold, mdiArrowRightBold, mdiClose, mdiFlagVariantOutline, mdiMagnify, mdiReplay } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useCallback, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { WithGameEditTab } from '@Components/admin/WithGameEditTab'
import { handleAxiosError } from '@Utils/ApiHelper'
import {
  currentFlagEgressBuffer,
  currentFlagEgressPage,
  flagEgressMatchesSearch,
  flagEgressPushIsCurrent,
  flagEgressSnapshotIsCurrent,
  formatFlagEgressAge,
  mergeFlagEgressRows,
  normalizeFlagEgressSearch,
  rebaseFlagEgressRows,
  type ScopedFlagEgressPage,
} from '@Utils/FlagEgressFeed'
import { useLanguage } from '@Utils/I18n'
import { LatestRequest } from '@Utils/LatestRequest'
import { OPERATOR_FALLBACK_POLL_MS } from '@Utils/SignalRRecovery'
import { useViewerIdentity } from '@Utils/ViewerIdentity'
import { useRecoveringHub } from '@Hooks/useRecoveringHub'
import api, { FlagEgressEventModel } from '@Api'
import tableClasses from '@Styles/Table.module.css'

const ITEMS_PER_PAGE = 50
const BACKFILL_PAGE_SIZE = 100
const MAX_BACKFILL_PAGES = 10
const MAX_BUFFERED_EVENTS = 200
const MAX_VISIBLE_EVENTS = 100

interface FlagEgressViewProps {
  gameId: number
  feedScope: string
}

const FlagEgressView: FC<FlagEgressViewProps> = ({ gameId, feedScope }) => {
  const { t } = useTranslation()
  const { locale } = useLanguage()
  const [activePage, setPage] = useState(1)
  const [search, setSearch] = useState('')
  const [debouncedSearch] = useDebouncedValue(search, 300)
  const normalizedSearch = normalizeFlagEgressSearch(debouncedSearch)
  const snapshotScope = JSON.stringify([feedScope, activePage, normalizedSearch])
  const reducedMotion = useReducedMotion()

  const [, update] = useState(0)
  const buffered = useRef<FlagEgressEventModel[]>([])
  const bufferedFeedScope = useRef(feedScope)
  const cursor = useRef(0)
  const cursorInitialized = useRef(false)
  const activeFeedScope = useRef(feedScope)
  const activeSnapshotScope = useRef(snapshotScope)
  const latestSnapshotRequest = useRef(0)
  const [snapshot, setSnapshot] = useState<ScopedFlagEgressPage>()
  const page = currentFlagEgressPage(snapshotScope, snapshot)
  const pageRequest = useRef(new LatestRequest())
  const recoveryRequest = useRef(new LatestRequest())
  const viewport = useRef<HTMLDivElement>(null)

  useEffect(() => {
    activeFeedScope.current = feedScope
    activeSnapshotScope.current = snapshotScope
  }, [feedScope, snapshotScope])

  useEffect(() => {
    viewport.current?.scrollTo({ top: 0, behavior: reducedMotion ? 'auto' : 'smooth' })
  }, [activePage, normalizedSearch, reducedMotion])

  const reportFetchError = useCallback(
    async (error: unknown) => {
      showNotification({
        color: 'red',
        title: t('admin.notification.flag_egress.fetch_failed', 'Could not load Flag Egress activity'),
        message: await handleAxiosError(error),
        icon: <Icon path={mdiClose} size={1} />,
        closeButtonProps: {
          'aria-label': t('common.button.close', 'Dismiss notification'),
        },
      })
    },
    [t]
  )

  const loadSnapshot = useCallback(async () => {
    const requestedAt = ++latestSnapshotRequest.current
    const response = await pageRequest.current.run((signal) =>
      api.admin.adminFlagEgressPage(
        gameId,
        {
          count: ITEMS_PER_PAGE,
          skip: (activePage - 1) * ITEMS_PER_PAGE,
          search: normalizedSearch || undefined,
        },
        { signal }
      )
    )
    if (
      !response ||
      !flagEgressSnapshotIsCurrent(
        activeSnapshotScope.current,
        snapshotScope,
        latestSnapshotRequest.current,
        requestedAt
      )
    ) {
      return
    }
    const nextPage = response.data
    setSnapshot({ scope: snapshotScope, page: nextPage })
    return nextPage
  }, [activePage, gameId, normalizedSearch, snapshotScope])

  useEffect(() => {
    void loadSnapshot().catch(reportFetchError)
    return () => pageRequest.current.cancel()
  }, [loadSnapshot, reportFetchError])

  useEffect(() => () => recoveryRequest.current.cancel(), [snapshotScope])

  const mergeIncoming = useCallback((incoming: readonly FlagEgressEventModel[]) => {
    if (incoming.length === 0) return
    buffered.current = mergeFlagEgressRows(incoming, buffered.current, MAX_BUFFERED_EVENTS)
    update((version) => version + 1)
  }, [])

  const rebaseAtCheckpoint = useCallback((checkpoint: number) => {
    buffered.current = rebaseFlagEgressRows(buffered.current, checkpoint)
    update((version) => version + 1)
  }, [])

  const reconcile = useCallback(
    () =>
      recoveryRequest.current.run(async (signal) => {
        const requestedFeedScope = feedScope
        const isCurrent = () => activeFeedScope.current === requestedFeedScope && !signal.aborted

        if (!cursorInitialized.current) {
          const checkpoint = await api.admin.adminFlagEgressBackfill(gameId, {}, { signal })
          if (!isCurrent()) return
          const authoritativePage = await loadSnapshot()
          if (!isCurrent() || authoritativePage === undefined) return
          rebaseAtCheckpoint(checkpoint.data.nextCursor)
          cursor.current = checkpoint.data.nextCursor
          cursorInitialized.current = true
          return
        }

        let after = cursor.current
        for (let pageIndex = 0; pageIndex < MAX_BACKFILL_PAGES && isCurrent(); pageIndex += 1) {
          const response = await api.admin.adminFlagEgressBackfill(
            gameId,
            { after, limit: BACKFILL_PAGE_SIZE },
            { signal }
          )
          if (!isCurrent()) return
          const backfill = response.data
          if (backfill.nextCursor < after || (backfill.nextCursor === after && backfill.hasMore)) {
            throw new Error('Flag Egress backfill cursor did not advance')
          }
          mergeIncoming(backfill.events)
          after = backfill.nextCursor
          cursor.current = after
          if (!backfill.hasMore) {
            await loadSnapshot()
            return
          }
        }

        if (!isCurrent()) return
        const checkpoint = await api.admin.adminFlagEgressBackfill(gameId, {}, { signal })
        if (!isCurrent()) return
        const authoritativePage = await loadSnapshot()
        if (!isCurrent() || authoritativePage === undefined) return
        rebaseAtCheckpoint(checkpoint.data.nextCursor)
        cursor.current = Math.max(cursor.current, checkpoint.data.nextCursor)
      }),
    [feedScope, gameId, loadSnapshot, mergeIncoming, rebaseAtCheckpoint]
  )

  useRecoveringHub({
    active: gameId > 0,
    url: `/hub/admin?feed=flagEgress&game=${gameId}`,
    ownerKey: feedScope,
    handlers: {
      ReceivedFlagEgress: (raw) => {
        const message = raw as FlagEgressEventModel
        if (
          !flagEgressPushIsCurrent(activeFeedScope.current, feedScope, message.gameId, gameId) ||
          (cursorInitialized.current && message.cursor <= cursor.current)
        ) {
          return
        }
        mergeIncoming([message])
      },
    },
    revalidate: reconcile,
    pollingIntervalMs: OPERATOR_FALLBACK_POLL_MS,
  })

  const currentBuffer = currentFlagEgressBuffer(feedScope, bufferedFeedScope.current, buffered.current)
  const filteredLive = currentBuffer.filter((event) => flagEgressMatchesSearch(event, normalizedSearch))
  const visibleEvents =
    activePage === 1
      ? mergeFlagEgressRows(filteredLive, page?.data ?? [], MAX_VISIBLE_EVENTS)
      : (page?.data ?? []).slice(0, ITEMS_PER_PAGE)
  const totalPages = Math.max(1, Math.ceil((page?.total ?? 0) / ITEMS_PER_PAGE))

  useEffect(() => {
    if (page && activePage > totalPages) setPage(totalPages)
  }, [activePage, page, totalPages])

  return (
    <WithGameEditTab
      isLoading={!page}
      head={
        <Group justify="space-between" w="100%" wrap="wrap">
          <TextInput
            w={{ base: '100%', sm: '36%' }}
            size="sm"
            aria-label={t('admin.placeholder.flag_egress.search', 'Filter by team, challenge, or IP')}
            leftSection={<Icon path={mdiMagnify} size={0.9} aria-hidden />}
            placeholder={t('admin.placeholder.flag_egress.search', 'Filter by team, challenge, or IP…')}
            value={search}
            onChange={(event) => {
              setSearch(event.currentTarget.value)
              setPage(1)
            }}
          />
          <Group gap="lg" wrap="wrap">
            <Stack gap={0} align="center" aria-live="polite">
              <Text fw={700} size="lg" c="red">
                {page?.total ?? 0}
              </Text>
              <Text size="xs" c="dimmed">
                {t('admin.label.flag_egress.total_events', 'Egress Events')}
              </Text>
            </Stack>
            <Group gap="xs" wrap="nowrap">
              <ActionIcon
                size="lg"
                disabled={activePage <= 1}
                aria-label={t('common.pagination.first', 'First page')}
                onClick={() => setPage(1)}
              >
                <Icon path={mdiReplay} size={1} />
              </ActionIcon>
              <ActionIcon
                size="lg"
                disabled={activePage <= 1}
                aria-label={t('common.pagination.previous', 'Previous page')}
                onClick={() => setPage((pageNumber) => Math.max(1, pageNumber - 1))}
              >
                <Icon path={mdiArrowLeftBold} size={1} />
              </ActionIcon>
              <Text size="sm" fw={700} aria-live="polite">
                {activePage} / {totalPages}
              </Text>
              <ActionIcon
                size="lg"
                disabled={activePage >= totalPages}
                aria-label={t('common.pagination.next', 'Next page')}
                onClick={() => setPage((pageNumber) => Math.min(totalPages, pageNumber + 1))}
              >
                <Icon path={mdiArrowRightBold} size={1} />
              </ActionIcon>
            </Group>
          </Group>
        </Group>
      }
    >
      {!page ? (
        <Center h="60vh">
          <Loader />
        </Center>
      ) : (
        <Paper shadow="md" p="xs" w="100%">
          <ScrollArea
            viewportRef={viewport}
            offsetScrollbars
            scrollbarSize={4}
            h="calc(100vh - 220px)"
            viewportProps={{
              tabIndex: 0,
              'aria-label': t('admin.content.flag_egress.table_caption', 'Recent flag egress activity'),
            }}
          >
            <Table className={tableClasses.table} highlightOnHover>
              <Table.Caption>
                {t('admin.content.flag_egress.table_caption', 'Recent flag egress activity')}
              </Table.Caption>
              <Table.Thead>
                <Table.Tr>
                  <Table.Th scope="col" miw={120}>
                    {t('admin.label.flag_egress.time', 'Last Seen')}
                  </Table.Th>
                  <Table.Th scope="col">{t('admin.label.flag_egress.team', 'Team')}</Table.Th>
                  <Table.Th scope="col">{t('admin.label.flag_egress.challenge', 'Challenge')}</Table.Th>
                  <Table.Th scope="col" miw={160}>
                    {t('admin.label.flag_egress.remote', 'Remote endpoint')}
                  </Table.Th>
                  <Table.Th scope="col" miw={80}>
                    {t('admin.label.flag_egress.hits', 'Hits')}
                  </Table.Th>
                </Table.Tr>
              </Table.Thead>
              <Table.Tbody>
                {visibleEvents.map((event) => (
                  <Table.Tr key={event.id}>
                    <Table.Td>
                      <Tooltip label={dayjs(event.lastSeenUtc).locale(locale).format('LLL')} withArrow>
                        <Text size="sm" ff="monospace" style={{ cursor: 'help' }}>
                          {formatFlagEgressAge(event.lastSeenUtc, locale)}
                        </Text>
                      </Tooltip>
                    </Table.Td>
                    <Table.Td>
                      <Group gap="xs">
                        <ThemeIcon size="xs" color="red" variant="light" radius="xl">
                          <Icon path={mdiFlagVariantOutline} size={0.6} aria-hidden />
                        </ThemeIcon>
                        <Text size="sm" fw={500}>
                          {event.teamName || `#${event.participationId}`}
                        </Text>
                      </Group>
                    </Table.Td>
                    <Table.Td>
                      <Text size="sm">{event.challengeTitle || `#${event.challengeId}`}</Text>
                    </Table.Td>
                    <Table.Td>
                      <Text size="sm" ff="monospace">
                        {event.remotePort > 0 ? `${event.remoteIp}:${event.remotePort}` : event.remoteIp}
                      </Text>
                    </Table.Td>
                    <Table.Td>
                      <Text size="sm" ff="monospace" c={event.hitCount > 10 ? 'red' : undefined}>
                        {event.hitCount}
                      </Text>
                    </Table.Td>
                  </Table.Tr>
                ))}
                {visibleEvents.length === 0 && (
                  <Table.Tr>
                    <Table.Td colSpan={5}>
                      <Text ta="center" c="dimmed" py="md" size="sm">
                        {normalizedSearch
                          ? t('admin.placeholder.flag_egress.no_match', 'No events match the filter.')
                          : t('admin.placeholder.flag_egress.empty', 'No flag-egress events yet.')}
                      </Text>
                    </Table.Td>
                  </Table.Tr>
                )}
              </Table.Tbody>
            </Table>
          </ScrollArea>
        </Paper>
      )}
    </WithGameEditTab>
  )
}

const FlagEgress: FC = () => {
  const { id } = useParams()
  const gameId = Number.parseInt(id ?? '-1', 10)
  const { scope: viewerScope } = useViewerIdentity()
  const feedScope = JSON.stringify([viewerScope, gameId])

  return <FlagEgressView key={feedScope} gameId={gameId} feedScope={feedScope} />
}

export default FlagEgress
