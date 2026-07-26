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
import * as signalR from '@microsoft/signalr'
import cx from 'clsx'
import dayjs from 'dayjs'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { AdminPage } from '@Components/admin/AdminPage'
import { handleAxiosError } from '@Utils/ApiHelper'
import { useLanguage } from '@Utils/I18n'
import { TaskStatusColorMap } from '@Utils/Shared'
import api, { LogMessageModel, TaskStatus } from '@Api'
import classes from '@Styles/AdminLogs.module.css'
import tableClasses from '@Styles/Table.module.css'

const ITEM_COUNT_PER_PAGE = 50

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
  const [level, setLevel] = useState(LogLevel.Info)
  const [activePage, setPage] = useState(1)
  const [search, setSearch] = useState('')
  const [debouncedSearch] = useDebouncedValue(search, 500)
  const theme = useMantineTheme()

  const [, update] = useState(new Date())
  const newLogs = useRef<LogMessageModel[]>([])
  const [logs, setLogs] = useState<LogMessageModel[]>()

  const { t } = useTranslation()
  const { locale } = useLanguage()
  const viewport = useRef<HTMLDivElement>(null)

  useEffect(() => {
    viewport.current?.scrollTo({ top: 0, behavior: 'smooth' })
  }, [activePage, level, viewport])

  useEffect(() => {
    const fetchLogs = async () => {
      try {
        const res = await api.admin.adminLogs({
          level,
          count: ITEM_COUNT_PER_PAGE,
          skip: (activePage - 1) * ITEM_COUNT_PER_PAGE,
          search: debouncedSearch || undefined,
        })
        setLogs(res.data)
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
    }

    fetchLogs()

    if (activePage === 1) {
      newLogs.current = []
    }
  }, [activePage, level, debouncedSearch])

  useEffect(() => {
    setPage(1)
  }, [level, debouncedSearch])

  useEffect(() => {
    const connection = new signalR.HubConnectionBuilder()
      .withUrl('/hub/admin')
      .withHubProtocol(new signalR.JsonHubProtocol())
      .withAutomaticReconnect()
      .configureLogging(signalR.LogLevel.None)
      .build()

    connection.serverTimeoutInMilliseconds = 60 * 1000 * 60 * 24

    connection.on('ReceivedLog', (message: LogMessageModel) => {
      newLogs.current = [message, ...newLogs.current]
      update(new Date(message.time!))
    })

    const startConnection = async () => {
      try {
        await connection.start()
        showNotification({
          color: 'teal',
          message: t('admin.notification.logs.connected'),
          icon: <Icon path={mdiCheck} size={1} />,
          closeButtonProps: {
            'aria-label': t('common.button.close', 'Dismiss notification'),
          },
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
  }, [])

  const visibleLogs = [...(activePage === 1 ? newLogs.current : []), ...(logs ?? [])].filter(
    (item) => level === 'All' || item.level === level
  )

  const rows = visibleLogs.map((item, i) => (
    <Table.Tr
      key={`${item.time}@${i}`}
      className={cx({
        [tableClasses.fade]:
          i === 0 && activePage === 1 && newLogs.current.length > 0 && newLogs.current[0].level === level,
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
            onChange={(value) => setLevel(value as LogLevel)}
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
            onChange={(e) => {
              setSearch(e.currentTarget.value)
              setPage(1)
            }}
            className={classes.search}
          />
          <Group justify="right">
            <ActionIcon
              size="lg"
              disabled={activePage <= 1}
              aria-label={t('common.pagination.previous', 'Previous page')}
              onClick={() => setPage(activePage - 1)}
            >
              <Icon path={mdiArrowLeftBold} size={1} />
            </ActionIcon>
            <Text fw="bold" size="sm">
              {activePage}
            </Text>
            <ActionIcon
              size="lg"
              disabled={!logs || logs.length < ITEM_COUNT_PER_PAGE}
              aria-label={t('common.pagination.next', 'Next page')}
              onClick={() => setPage(activePage + 1)}
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
          visibleLogs.map((item, index) => (
            <Paper component="article" key={`${item.time}@${index}`} p="md" withBorder className={classes.logCard}>
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
