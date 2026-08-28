import {
  ActionIcon,
  Badge,
  Group,
  Input,
  Paper,
  ScrollArea,
  SegmentedControl,
  Table,
  TextInput,
  Tooltip,
  VisuallyHidden,
  useMantineColorScheme,
  useMantineTheme,
} from '@mantine/core'
import { useDebouncedValue } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import {
  mdiArrowLeftBold,
  mdiArrowRightBold,
  mdiCheck,
  mdiClose,
  mdiCrosshairsQuestion,
  mdiDotsHorizontal,
  mdiDownload,
  mdiExclamationThick,
  mdiFlag,
  mdiMagnify,
  mdiReplay,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import cx from 'clsx'
import dayjs from 'dayjs'
import { FC, useCallback, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { ScrollingText } from '@Components/ScrollingText'
import { WithGameMonitor } from '@Components/WithGameMonitor'
import { downloadBlob, handleAxiosError } from '@Utils/ApiHelper'
import { useLanguage } from '@Utils/I18n'
import { LatestRequest } from '@Utils/LatestRequest'
import {
  currentMonitorBufferRows,
  currentMonitorSnapshotRows,
  mergeSubmissionBuffer,
  monitorCursorPushIsCurrent,
  monitorSnapshotIsCurrent,
  rebaseSubmissionBuffer,
  type ScopedMonitorSnapshot,
  submissionMatchesMonitorFilter,
  submissionMonitorIdentity,
  unreconciledMonitorRows,
} from '@Utils/MonitorFeed'
import { OPERATOR_FALLBACK_POLL_MS } from '@Utils/SignalRRecovery'
import { useDisplayInputStyles } from '@Utils/ThemeOverride'
import { useViewerIdentity } from '@Utils/ViewerIdentity'
import { useGame, useGameStatus, useRevalidateWhenPollingStops } from '@Hooks/useGame'
import { useRecoveringHub } from '@Hooks/useRecoveringHub'
import api, { AnswerResult, MonitorSubmission } from '@Api'
import tableClasses from '@Styles/Table.module.css'

const ITEM_COUNT_PER_PAGE = 50
const BACKFILL_PAGE_SIZE = 100
const MAX_BACKFILL_PAGES = 10
const MAX_BUFFERED_SUBMISSIONS = 500

const AnswerResultMap = new Map([
  [AnswerResult.Accepted, 'AC'],
  [AnswerResult.WrongAnswer, 'WA'],
  [AnswerResult.CheatDetected, 'CD'],
  [AnswerResult.NotFound, 'NF'],
])

const AnswerResultIconMap = (size: number) => {
  const theme = useMantineTheme()
  const { colorScheme } = useMantineColorScheme()

  const colorIdx = colorScheme === 'dark' ? 4 : 7

  return new Map([
    [AnswerResult.Accepted, { path: mdiCheck, size, color: theme.colors.green[colorIdx] }],
    [AnswerResult.WrongAnswer, { path: mdiClose, size, color: theme.colors.red[colorIdx] }],
    [AnswerResult.NotFound, { path: mdiCrosshairsQuestion, size, color: theme.colors.gray[colorIdx] }],
    [AnswerResult.CheatDetected, { path: mdiExclamationThick, size, color: theme.colors.orange[colorIdx] }],
    [AnswerResult.FlagSubmitted, { path: mdiDotsHorizontal, size, color: theme.colors.gray[colorIdx] }],
  ])
}

const Submissions: FC = () => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')

  const [activePage, setPage] = useState(1)
  const [search, setSearch] = useState('')
  const [debouncedSearch] = useDebouncedValue(search, 500)
  const [type, setType] = useState<AnswerResult | 'All'>('All')
  const { scope: viewerScope } = useViewerIdentity()
  const feedScope = JSON.stringify([viewerScope, numId])
  const snapshotScope = JSON.stringify([feedScope, activePage, type, debouncedSearch])

  const [, update] = useState(0)
  const newSubmissions = useRef<MonitorSubmission[]>([])
  const bufferedFeedScope = useRef(feedScope)
  const submissionCursor = useRef(0)
  const cursorInitialized = useRef(false)
  const activeFeedScope = useRef(feedScope)
  const activeSnapshotScope = useRef(snapshotScope)
  const latestSnapshotRequest = useRef(0)
  const [submissionSnapshot, setSubmissionSnapshot] = useState<ScopedMonitorSnapshot<MonitorSubmission>>()
  const submissions = currentMonitorSnapshotRows(snapshotScope, submissionSnapshot)
  const submissionRequest = useRef(new LatestRequest())
  const recoveryRequest = useRef(new LatestRequest())
  const [disabled, setDisabled] = useState(false)

  const { game } = useGame(numId)
  const { finished, status: gameStatus } = useGameStatus(game)
  const monitorConnectionActive = Boolean(game?.end) && !finished

  const iconMap = AnswerResultIconMap(0.8)
  const { classes: inputClasses } = useDisplayInputStyles({ ff: 'monospace' })
  const theme = useMantineTheme()

  const { t } = useTranslation()
  const { locale } = useLanguage()
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
    const res = await submissionRequest.current.run((signal) =>
      api.game.gameSubmissionPage(
        numId,
        {
          type: type === 'All' ? undefined : type,
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
    setSubmissionSnapshot({ scope: snapshotScope, rows: res.data })
    return res.data
  }, [activePage, type, debouncedSearch, numId, snapshotScope])

  const fetchSubmissions = useCallback(async () => {
    try {
      return await loadSnapshot()
    } catch (err) {
      showNotification({
        color: 'red',
        title: t('game.notification.fetch_failed.submission'),
        message: await handleAxiosError(err),
        icon: <Icon path={mdiClose} size={1} />,
      })
    }
  }, [loadSnapshot, t])

  const mergeIncomingSubmissions = useCallback((incoming: readonly MonitorSubmission[]) => {
    if (incoming.length === 0) return
    newSubmissions.current = mergeSubmissionBuffer(incoming, newSubmissions.current, MAX_BUFFERED_SUBMISSIONS)
    update((version) => version + 1)
  }, [])

  const rebaseAtCheckpoint = useCallback((checkpoint: number) => {
    newSubmissions.current = rebaseSubmissionBuffer(newSubmissions.current, checkpoint)
    update((version) => version + 1)
  }, [])

  useEffect(() => {
    submissionCursor.current = 0
    cursorInitialized.current = false
    newSubmissions.current = []
    bufferedFeedScope.current = feedScope
    setSubmissionSnapshot(undefined)
    update((version) => version + 1)
    return () => {
      submissionRequest.current.cancel()
      recoveryRequest.current.cancel()
    }
  }, [feedScope])

  useEffect(() => {
    void fetchSubmissions()

    if (activePage === 1) {
      newSubmissions.current = []
    }

    return () => submissionRequest.current.cancel()
  }, [activePage, fetchSubmissions])

  const reconcileSubmissions = useCallback(
    () =>
      recoveryRequest.current.run(async (signal) => {
        const requestedFeedScope = feedScope
        const isCurrent = () => activeFeedScope.current === requestedFeedScope && !signal.aborted

        if (!cursorInitialized.current) {
          const checkpoint = await api.game.gameSubmissionBackfill(numId, {}, { signal })
          if (!isCurrent()) return
          const snapshot = await loadSnapshot()
          if (!isCurrent() || snapshot === undefined) return
          rebaseAtCheckpoint(checkpoint.data.nextCursor)
          submissionCursor.current = checkpoint.data.nextCursor
          cursorInitialized.current = true
          return
        }

        let cursor = submissionCursor.current
        for (let page = 0; page < MAX_BACKFILL_PAGES && isCurrent(); page += 1) {
          const response = await api.game.gameSubmissionBackfill(
            numId,
            { after: cursor, limit: BACKFILL_PAGE_SIZE },
            { signal }
          )
          if (!isCurrent()) return
          if (response.data.nextCursor < cursor || (response.data.nextCursor === cursor && response.data.hasMore)) {
            throw new Error('Monitor submission backfill cursor did not advance')
          }
          mergeIncomingSubmissions(response.data.submissions)
          cursor = response.data.nextCursor
          submissionCursor.current = cursor
          if (!response.data.hasMore) {
            await loadSnapshot()
            return
          }
        }

        if (!isCurrent()) return
        // Cap recovery at ten pages so an idle tab cannot monopolize the API.
        // A fresh authoritative snapshot/checkpoint replaces an older gap.
        const checkpoint = await api.game.gameSubmissionBackfill(numId, {}, { signal })
        if (!isCurrent()) return
        const snapshot = await loadSnapshot()
        if (!isCurrent() || snapshot === undefined) return
        rebaseAtCheckpoint(checkpoint.data.nextCursor)
        submissionCursor.current = Math.max(submissionCursor.current, checkpoint.data.nextCursor)
      }),
    [feedScope, loadSnapshot, mergeIncomingSubmissions, numId, rebaseAtCheckpoint]
  )

  const { waitForStop: waitForMonitorHubStop } = useRecoveringHub({
    active: monitorConnectionActive,
    url: `/hub/monitor?game=${numId}`,
    ownerKey: gameStatus,
    handlers: {
      ReceivedSubmissions: (raw) => {
        const message = raw as MonitorSubmission
        if (
          !monitorCursorPushIsCurrent(
            activeFeedScope.current,
            feedScope,
            false,
            cursorInitialized.current,
            submissionCursor.current,
            message.cursor
          )
        )
          return
        mergeIncomingSubmissions([message])
      },
    },
    revalidate: reconcileSubmissions,
    pollingIntervalMs: OPERATOR_FALLBACK_POLL_MS,
    onConnected: () =>
      showNotification({
        color: 'teal',
        message: t('game.notification.connected.submission'),
        icon: <Icon path={mdiCheck} size={1} />,
      }),
  })

  // Keep the final request separate from hub ownership and fence it behind the
  // completed stop. A commit whose boundary broadcast loses the listener is
  // therefore present in the one post-stop authoritative reconciliation.
  useRevalidateWhenPollingStops(monitorConnectionActive, reconcileSubmissions, waitForMonitorHubStop)

  const currentBufferedSubmissions = currentMonitorBufferRows(
    feedScope,
    bufferedFeedScope.current,
    newSubmissions.current
  )
  const filteredSubs = currentBufferedSubmissions.filter((item) =>
    submissionMatchesMonitorFilter(item, type, debouncedSearch)
  )
  const bufferedSubmissions =
    activePage === 1 ? unreconciledMonitorRows(filteredSubs, submissions ?? [], submissionMonitorIdentity) : []
  const visibleSubmissions =
    activePage === 1
      ? mergeSubmissionBuffer(bufferedSubmissions, submissions ?? [], ITEM_COUNT_PER_PAGE)
      : (submissions ?? [])

  const rows = visibleSubmissions.map((item, i) => (
    <Table.Tr key={item.id} className={cx({ [tableClasses.fade]: i === 0 && bufferedSubmissions.length > 0 })}>
      <Table.Td>
        <Icon {...iconMap.get(item.status)!} />
        <VisuallyHidden>{item.status}</VisuallyHidden>
      </Table.Td>
      <Table.Td ff="monospace">
        <Badge size="sm" color="indigo" fullWidth>
          {dayjs(item.time).locale(locale).format('SL HH:mm:ss')}
        </Badge>
      </Table.Td>
      <Table.Td>
        <ScrollingText text={item.team ?? 'Team'} size="sm" fw="bold" maw={150} />
      </Table.Td>
      <Table.Td>
        <ScrollingText text={item.user ?? 'User'} ff="monospace" size="sm" fw="bold" maw={150} />
      </Table.Td>
      <Table.Td>{item.challenge ?? 'Challenge'}</Table.Td>
      <Table.Td w="36vw" maw="100%" p="0">
        <Input
          variant="unstyled"
          value={item.answer}
          aria-label={t('game.label.submissions.answer', 'Submitted answer')}
          readOnly
          size="sm"
          classNames={inputClasses}
        />
      </Table.Td>
    </Table.Tr>
  ))

  const onDownloadSubmissionSheet = () =>
    downloadBlob(
      `monitor:submissions:${numId}`,
      () => api.game.gameSubmissionSheet(numId, { format: 'blob' }),
      setDisabled,
      t,
      `Submission_${numId}_${Date.now()}.xlsx`
    )

  return (
    <WithGameMonitor isLoading={!submissions}>
      <Group justify="space-between" w="100%" mb="sm" align="flex-end">
        <Input.Wrapper label={t('game.label.submissions.result_filter', 'Result')}>
          <SegmentedControl
            color={theme.primaryColor}
            aria-label={t('game.label.submissions.result_filter', 'Result')}
            value={type}
            bg="transparent"
            onChange={(value) => {
              setType(value as AnswerResult | 'All')
              setPage(1)
            }}
            data={[
              {
                label: 'All',
                value: 'All',
              },
              ...Object.entries(AnswerResult)
                .map((role) => ({
                  value: role[1],
                  label: AnswerResultMap.get(role[1]),
                }))
                .filter((role) => role.value !== AnswerResult.FlagSubmitted),
            ]}
          />
        </Input.Wrapper>
        <TextInput
          label={t('game.label.submissions.search', 'Search submissions')}
          placeholder={t('game.label.submissions.search_placeholder', 'Search submissions...')}
          leftSection={<Icon path={mdiMagnify} size={0.8} />}
          value={search}
          onChange={(e) => {
            setSearch(e.currentTarget.value)
            setPage(1)
          }}
          style={{ width: 250 }}
        />
        <Group justify="right">
          <Tooltip label={t('game.button.download.submissionsheet')} position="left">
            <ActionIcon
              aria-label={t('game.button.download.submissionsheet', 'Download submissions')}
              disabled={disabled}
              size="lg"
              onClick={onDownloadSubmissionSheet}
            >
              <Icon path={mdiDownload} size={1} />
            </ActionIcon>
          </Tooltip>
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
            disabled={submissions && submissions.length < ITEM_COUNT_PER_PAGE}
            onClick={() => setPage(activePage + 1)}
          >
            <Icon path={mdiArrowRightBold} size={1} />
          </ActionIcon>
        </Group>
      </Group>
      <Paper shadow="md" p="md">
        <ScrollArea
          viewportRef={viewport}
          offsetScrollbars
          h="calc(100vh - 200px)"
          viewportProps={{
            tabIndex: 0,
            'aria-label': t('game.label.submissions.table_caption', 'Game submissions'),
          }}
        >
          <Table className={tableClasses.table}>
            <Table.Caption>
              <VisuallyHidden>{t('game.label.submissions.table_caption', 'Game submissions')}</VisuallyHidden>
            </Table.Caption>
            <Table.Thead>
              <Table.Tr>
                <Table.Th scope="col" w="0.6rem">
                  <Group align="center">
                    <Icon path={mdiFlag} size={0.8} />
                    <VisuallyHidden>{t('common.label.status')}</VisuallyHidden>
                  </Group>
                </Table.Th>
                <Table.Th scope="col" w="7rem">
                  {t('common.label.time')}
                </Table.Th>
                <Table.Th scope="col" miw="4.5rem">
                  {t('common.label.team')}
                </Table.Th>
                <Table.Th scope="col" miw="4.5rem">
                  {t('common.label.user')}
                </Table.Th>
                <Table.Th scope="col" miw="3rem">
                  {t('common.label.challenge')}
                </Table.Th>
                <Table.Th scope="col" ff="monospace">
                  {t('common.label.flag')}
                </Table.Th>
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>{rows}</Table.Tbody>
          </Table>
        </ScrollArea>
      </Paper>
    </WithGameMonitor>
  )
}

export default Submissions
