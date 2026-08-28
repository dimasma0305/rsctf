import {
  ActionIcon,
  Badge,
  Group,
  Paper,
  ScrollArea,
  SegmentedControl,
  SimpleGrid,
  Stack,
  Table,
  Text,
  TextInput,
  useMantineTheme,
} from '@mantine/core'
import { useDebouncedValue } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import { mdiArrowLeftBold, mdiArrowRightBold, mdiCheck, mdiClose, mdiMagnify } from '@mdi/js'
import { Icon } from '@mdi/react'
import cx from 'clsx'
import dayjs from 'dayjs'
import { FC, useCallback, useEffect, useReducer, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { AdminPage } from '@Components/admin/AdminPage'
import {
  ADMIN_LOG_PAGE_SIZE,
  adminLogIdentity,
  adminLogMatchesQuery,
  adminLogQueryReducer,
  adminLogQueryScope,
  compareAdminLogsNewestFirst,
  MAX_BUFFERED_ADMIN_LOGS,
  MAX_VISIBLE_ADMIN_LOGS,
} from '@Utils/AdminLogFeed'
import { handleAxiosError } from '@Utils/ApiHelper'
import { mergeUniqueRows, prependUniqueBoundedRow, reconcileLiveRows } from '@Utils/FeedReconciliation'
import { useLanguage } from '@Utils/I18n'
import { currentListSnapshotRows, LatestListRequest, type ListSnapshot } from '@Utils/LatestRequest'
import { TaskStatusColorMap } from '@Utils/Shared'
import { OPERATOR_FALLBACK_POLL_MS } from '@Utils/SignalRRecovery'
import { useRecoveringHub } from '@Hooks/useRecoveringHub'
import api, { LogMessageModel, TaskStatus } from '@Api'
import classes from '@Styles/AdminLogs.module.css'
import tableClasses from '@Styles/Table.module.css'

enum LogLevel {
  Info = 'Information',
  Warn = 'Warning',
  Error = 'Error',
  All = 'All',
}

const LOG_LEVEL_COLOR: Record<string, string> = {
  Information: 'blue',
  Warning: 'yellow',
  Error: 'red',
}

const Logs: FC = () => {
  const [search, setSearch] = useState('')
  const [debouncedSearch] = useDebouncedValue(search, 500)
  const [query, dispatchQuery] = useReducer(adminLogQueryReducer, {
    level: LogLevel.Info,
    page: 1,
    search: '',
  })
  const { level, page: activePage } = query
  const queryScope = adminLogQueryScope(query)
  const queryReady = search === query.search
  const theme = useMantineTheme()

  const [, update] = useState(0)
  const newLogs = useRef<LogMessageModel[]>([])
  const logRequest = useRef(new LatestListRequest<LogMessageModel>())
  const [logSnapshot, setLogSnapshot] = useState<ListSnapshot<LogMessageModel>>()
  const logs = queryReady ? currentListSnapshotRows(queryScope, logSnapshot) : undefined

  const { t } = useTranslation()
  const { locale } = useLanguage()
  const viewport = useRef<HTMLDivElement>(null)

  useEffect(() => {
    viewport.current?.scrollTo({ top: 0, behavior: 'smooth' })
  }, [activePage, level, viewport])

  useEffect(() => {
    dispatchQuery({ type: 'search', search: debouncedSearch })
  }, [debouncedSearch])

  const fetchLogs = useCallback(async () => {
    if (!queryReady) return
    const snapshot = await logRequest.current.run(queryScope, async (signal) => {
      const res = await api.admin.adminLogs(
        {
          level,
          count: ADMIN_LOG_PAGE_SIZE,
          skip: (activePage - 1) * ADMIN_LOG_PAGE_SIZE,
          search: query.search || undefined,
        },
        { signal }
      )
      return res.data.slice(0, ADMIN_LOG_PAGE_SIZE)
    })
    if (!snapshot) return

    newLogs.current = reconcileLiveRows(newLogs.current, snapshot.rows, adminLogIdentity).slice(
      0,
      MAX_BUFFERED_ADMIN_LOGS
    )
    setLogSnapshot(snapshot)
  }, [activePage, level, query.search, queryReady, queryScope])

  const fetchLogsForUi = useCallback(async () => {
    try {
      await fetchLogs()
    } catch (err) {
      showNotification({
        color: 'red',
        title: t('admin.notification.logs.fetch_failed'),
        message: await handleAxiosError(err),
        icon: <Icon path={mdiClose} size={1} />,
        closeButtonProps: {
          'aria-label': t('common.button.close', 'Dismiss notification'),
        },
      })
    }
  }, [fetchLogs, t])

  useEffect(() => {
    void fetchLogsForUi()
    return () => logRequest.current.cancel()
  }, [fetchLogsForUi])

  useRecoveringHub({
    active: true,
    url: '/hub/admin',
    handlers: {
      ReceivedLog: (raw) => {
        const message = raw as LogMessageModel
        newLogs.current = prependUniqueBoundedRow(message, newLogs.current, MAX_BUFFERED_ADMIN_LOGS, adminLogIdentity)
        update((version) => version + 1)
      },
    },
    revalidate: fetchLogs,
    pollingIntervalMs: OPERATOR_FALLBACK_POLL_MS,
    onConnected: () =>
      showNotification({
        color: 'teal',
        message: t('admin.notification.logs.connected'),
        icon: <Icon path={mdiCheck} size={1} />,
        closeButtonProps: {
          'aria-label': t('common.button.close', 'Dismiss notification'),
        },
      }),
  })

  const bufferedLogs =
    queryReady && activePage === 1 ? newLogs.current.filter((item) => adminLogMatchesQuery(item, query)) : []
  const snapshotLogs = (logs ?? []).filter((item) => adminLogMatchesQuery(item, query))
  const visibleLogs = mergeUniqueRows(bufferedLogs, snapshotLogs, adminLogIdentity, MAX_VISIBLE_ADMIN_LOGS).sort(
    compareAdminLogsNewestFirst
  )

  const rows = visibleLogs.map((item, i) => (
    <Table.Tr
      key={item.id}
      className={cx({
        [tableClasses.fade]: i === 0 && bufferedLogs.length > 0 && bufferedLogs[0].id === item.id,
      })}
    >
      <Table.Td className={tableClasses.time}>
        <Badge size="sm" color="indigo" fullWidth autoContrast>
          <time dateTime={item.time ? new Date(item.time).toISOString() : undefined}>
            {dayjs(item.time).locale(locale).format('SL HH:mm:ss')}
          </time>
        </Badge>
      </Table.Td>
      <Table.Td>
        <Text ff="monospace" size="sm" fw={500} className={classes.cellText} title={item.ip || undefined}>
          {item.ip || ''}
        </Text>
      </Table.Td>
      <Table.Td>
        <Text ff="monospace" size="sm" fw="bold" className={classes.cellText} title={item.name || undefined}>
          {item.name || ''}
        </Text>
      </Table.Td>
      <Table.Td>
        <Text ff="monospace" size="sm" c="dimmed" className={classes.cellText} title={item.fingerprint || undefined}>
          {item.fingerprint || ''}
        </Text>
      </Table.Td>
      <Table.Td>
        <Text size="sm" className={classes.messageText} title={item.msg || undefined}>
          {item.msg || ''}
        </Text>
      </Table.Td>
      <Table.Td ff="monospace">
        {item.status && (
          <Badge size="sm" color={TaskStatusColorMap.get(item.status as TaskStatus) ?? 'gray'} autoContrast>
            {item.status}
          </Badge>
        )}
      </Table.Td>
    </Table.Tr>
  ))

  return (
    <AdminPage
      isLoading={!logs}
      head={
        <>
          <SegmentedControl
            aria-label={t('admin.label.logs.level_filter', 'Filter logs by level')}
            color={theme.primaryColor}
            value={level}
            bg="transparent"
            onChange={(value) => dispatchQuery({ type: 'level', level: value as LogLevel })}
            data={Object.entries(LogLevel).map((role) => ({
              value: role[1],
              label: role[0],
            }))}
          />
          <TextInput
            aria-label={t('admin.label.logs.search', 'Search logs')}
            placeholder={t('admin.label.logs.search', 'Search logs')}
            leftSection={<Icon path={mdiMagnify} size={0.8} />}
            value={search}
            onChange={(e) => setSearch(e.currentTarget.value)}
            className={classes.search}
          />
          <Group justify="right">
            <ActionIcon
              size="lg"
              disabled={!queryReady || activePage <= 1}
              aria-label={t('common.pagination.previous', 'Previous page')}
              onClick={() => dispatchQuery({ type: 'page', page: activePage - 1 })}
            >
              <Icon path={mdiArrowLeftBold} size={1} />
            </ActionIcon>
            <Text fw="bold" size="sm">
              {activePage}
            </Text>
            <ActionIcon
              size="lg"
              disabled={!logs || logs.length < ADMIN_LOG_PAGE_SIZE}
              aria-label={t('common.pagination.next', 'Next page')}
              onClick={() => dispatchQuery({ type: 'page', page: activePage + 1 })}
            >
              <Icon path={mdiArrowRightBold} size={1} />
            </ActionIcon>
          </Group>
        </>
      }
    >
      <Paper shadow="md" p="md" w="100%" visibleFrom="md" className={classes.tablePaper}>
        <ScrollArea
          viewportRef={viewport}
          offsetScrollbars
          scrollbarSize={8}
          h="calc(100vh - 190px)"
          viewportProps={{
            tabIndex: 0,
            'aria-label': t('admin.content.logs.scroll_label', 'Scrollable administrative activity logs'),
          }}
        >
          <Table className={cx(tableClasses.table, tableClasses.fixed)}>
            <Table.Caption>{t('admin.content.logs.table_caption', 'Administrative activity logs')}</Table.Caption>
            <Table.Thead>
              <Table.Tr>
                <Table.Th scope="col" w="7rem">
                  {t('common.label.time')}
                </Table.Th>
                <Table.Th scope="col" w="9rem">
                  {t('common.label.ip')}
                </Table.Th>
                <Table.Th scope="col" w="7rem">
                  {t('common.label.user')}
                </Table.Th>
                <Table.Th scope="col" w="7rem">
                  {t('common.label.fingerprint')}
                </Table.Th>
                <Table.Th scope="col" w="100%">
                  {t('admin.label.logs.message')}
                </Table.Th>
                <Table.Th scope="col" w="6rem">
                  {t('admin.label.logs.status')}
                </Table.Th>
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>{rows}</Table.Tbody>
          </Table>
        </ScrollArea>
      </Paper>

      <Stack hiddenFrom="md" gap="sm" w="100%" aria-label={t('admin.content.logs.table_caption')}>
        {visibleLogs.length === 0 ? (
          <Paper p="xl" withBorder className={classes.emptyState}>
            <Text fw={700}>{t('admin.content.logs.empty_title', 'No matching activity')}</Text>
            <Text size="sm" c="dimmed">
              {t('admin.content.logs.empty_description', 'Try another level or search term.')}
            </Text>
          </Paper>
        ) : (
          visibleLogs.map((item) => (
            <Paper component="article" key={item.id} p="md" withBorder className={classes.logCard}>
              <Stack gap="sm">
                <Group justify="space-between" align="flex-start" gap="sm" wrap="nowrap">
                  <Group gap="xs" wrap="wrap">
                    <Badge color={LOG_LEVEL_COLOR[item.level ?? ''] ?? 'gray'} variant="light" size="sm">
                      {item.level || t('admin.content.logs.level_unknown', 'Activity')}
                    </Badge>
                    {item.status && (
                      <Badge
                        color={TaskStatusColorMap.get(item.status as TaskStatus) ?? 'gray'}
                        variant="light"
                        size="sm"
                        autoContrast
                      >
                        {item.status}
                      </Badge>
                    )}
                  </Group>
                  <Text size="xs" c="dimmed" ff="monospace" className={classes.cardTime}>
                    <time dateTime={item.time ? new Date(item.time).toISOString() : undefined}>
                      {dayjs(item.time).locale(locale).format('SL HH:mm:ss')}
                    </time>
                  </Text>
                </Group>

                <Text size="sm" className={classes.cardMessage}>
                  {item.msg || t('admin.content.logs.no_message', 'No message recorded.')}
                </Text>

                {(item.name || item.ip || item.fingerprint) && (
                  <SimpleGrid component="dl" cols={2} spacing="sm" className={classes.cardMetadata}>
                    {item.name && (
                      <div>
                        <Text component="dt" className={classes.metaLabel}>
                          {t('common.label.user')}
                        </Text>
                        <Text component="dd" className={classes.metaValue} title={item.name}>
                          {item.name}
                        </Text>
                      </div>
                    )}
                    {item.ip && (
                      <div>
                        <Text component="dt" className={classes.metaLabel}>
                          {t('common.label.ip')}
                        </Text>
                        <Text component="dd" className={classes.metaValue} title={item.ip}>
                          {item.ip}
                        </Text>
                      </div>
                    )}
                    {item.fingerprint && (
                      <div className={classes.metadataWide}>
                        <Text component="dt" className={classes.metaLabel}>
                          {t('common.label.fingerprint')}
                        </Text>
                        <Text component="dd" className={classes.metaValue} title={item.fingerprint}>
                          {item.fingerprint}
                        </Text>
                      </div>
                    )}
                  </SimpleGrid>
                )}
              </Stack>
            </Paper>
          ))
        )}
      </Stack>
    </AdminPage>
  )
}

export default Logs
