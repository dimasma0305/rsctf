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
import { submissionMonitorIdentity, unreconciledMonitorRows } from '@Utils/MonitorFeed'
import { OPERATOR_FALLBACK_POLL_MS } from '@Utils/SignalRRecovery'
import { useDisplayInputStyles } from '@Utils/ThemeOverride'
import { useGame, useGameStatus, useRevalidateWhenPollingStops } from '@Hooks/useGame'
import { useRecoveringHub } from '@Hooks/useRecoveringHub'
import api, { AnswerResult, Submission } from '@Api'
import tableClasses from '@Styles/Table.module.css'

const ITEM_COUNT_PER_PAGE = 50

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

  const [, update] = useState(new Date())
  const newSubmissions = useRef<Submission[]>([])
  const [submissions, setSubmissions] = useState<Submission[]>()
  const submissionRequest = useRef(new LatestRequest())
  const [type, setType] = useState<AnswerResult | 'All'>('All')
  const [disabled, setDisabled] = useState(false)

  const { game } = useGame(numId)
  const { finished } = useGameStatus(game)
  const monitorConnectionActive = Boolean(game?.end) && !finished

  const iconMap = AnswerResultIconMap(0.8)
  const { classes: inputClasses } = useDisplayInputStyles({ ff: 'monospace' })
  const theme = useMantineTheme()

  const { t } = useTranslation()
  const { locale } = useLanguage()
  const viewport = useRef<HTMLDivElement>(null)

  useEffect(() => {
    viewport.current?.scrollTo({ top: 0, behavior: 'smooth' })
  }, [activePage, viewport])

  const fetchSubmissions = useCallback(async () => {
    try {
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
      if (!res) return
      newSubmissions.current = unreconciledMonitorRows(newSubmissions.current, res.data, submissionMonitorIdentity)
      setSubmissions(res.data)
    } catch (err) {
      showNotification({
        color: 'red',
        title: t('game.notification.fetch_failed.submission'),
        message: await handleAxiosError(err),
        icon: <Icon path={mdiClose} size={1} />,
      })
    }
  }, [activePage, type, debouncedSearch, numId, t])

  useEffect(() => {
    void fetchSubmissions()

    if (activePage === 1) {
      newSubmissions.current = []
    }

    return () => submissionRequest.current.cancel()
  }, [activePage, fetchSubmissions])

  const { waitForStop: waitForMonitorHubStop } = useRecoveringHub({
    active: monitorConnectionActive,
    url: `/hub/monitor?game=${numId}`,
    handlers: {
      ReceivedSubmissions: (raw) => {
        const message = raw as Submission
        newSubmissions.current = [message, ...newSubmissions.current]
        update(new Date(message.time!))
      },
    },
    revalidate: fetchSubmissions,
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
  useRevalidateWhenPollingStops(monitorConnectionActive, fetchSubmissions, waitForMonitorHubStop)

  const filteredSubs = newSubmissions.current.filter((item) => type === 'All' || item.status === type)
  const bufferedSubmissions =
    activePage === 1 ? unreconciledMonitorRows(filteredSubs, submissions ?? [], submissionMonitorIdentity) : []

  const rows = [...bufferedSubmissions, ...(submissions ?? [])].map((item, i) => (
    <Table.Tr
      key={`${item.time}@${i}`}
      className={cx({ [tableClasses.fade]: i === 0 && bufferedSubmissions.length > 0 })}
    >
      <Table.Td>
        <Icon {...iconMap.get(item.status ?? AnswerResult.FlagSubmitted)!} />
        <VisuallyHidden>{item.status ?? AnswerResult.FlagSubmitted}</VisuallyHidden>
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
