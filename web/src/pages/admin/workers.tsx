import {
  ActionIcon,
  Badge,
  Button,
  Group,
  Menu,
  Paper,
  Select,
  SimpleGrid,
  Stack,
  Table,
  Text,
  TextInput,
  ThemeIcon,
  Title,
  Tooltip,
} from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import {
  mdiCheck,
  mdiCheckCircleOutline,
  mdiDotsHorizontal,
  mdiKeyChange,
  mdiMagnify,
  mdiPackageVariantClosed,
  mdiPlus,
  mdiRefresh,
  mdiServerNetwork,
  mdiTrashCanOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import relativeTime from 'dayjs/plugin/relativeTime'
import { FC, ReactNode, useCallback, useEffect, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Empty } from '@Components/Empty'
import { AdminPage } from '@Components/admin/AdminPage'
import { WorkerDialogs } from '@Components/admin/workers/WorkerDialogs'
import { WorkerRetirement } from '@Components/admin/workers/WorkerRetirement'
import {
  CreatedWorker,
  Enrollment,
  Worker,
  WorkerFilter,
  WorkerInstallCommands,
  WorkerState,
} from '@Components/admin/workers/types'
import { showErrorMsg } from '@Utils/Shared'
import {
  workerInstallCommand,
  workerUninstallCommand,
  workerWindowsInstallCommand,
  workerWindowsUninstallCommand,
} from '@Utils/WorkerInstall'
import api, { ContentType } from '@Api'
import classes from '@Styles/AdminWorkers.module.css'

dayjs.extend(relativeTime)

interface SummaryMetricProps {
  label: string
  value: number
  helper: string
  color: string
  icon: string
}

const SummaryMetric: FC<SummaryMetricProps> = ({ label, value, helper, color, icon }) => (
  <Paper component="article" withBorder p="md" className={classes.metric}>
    <Group justify="space-between" align="flex-start" wrap="nowrap">
      <Stack gap={2}>
        <Text size="xs" fw={750} tt="uppercase" c="dimmed" className={classes.metricLabel}>
          {label}
        </Text>
        <Text className={classes.metricValue}>{value}</Text>
        <Text size="xs" c="dimmed" className={classes.metricHelper}>
          {helper}
        </Text>
      </Stack>
      <ThemeIcon color={color} variant="light" size={42} radius="md">
        <Icon path={icon} size={1.05} aria-hidden="true" />
      </ThemeIcon>
    </Group>
  </Paper>
)

const formatMemory = (bytes: number): string => {
  if (!Number.isFinite(bytes) || bytes <= 0) return '—'

  const gibibytes = bytes / 1024 ** 3
  return `${gibibytes >= 10 ? Math.round(gibibytes) : gibibytes.toFixed(1)} GiB`
}

const formatCpu = (cpuMillis: number): string => {
  if (!Number.isFinite(cpuMillis) || cpuMillis <= 0) return '—'

  const cores = cpuMillis / 1000
  return `${Number.isInteger(cores) ? cores : cores.toFixed(1)} vCPU`
}

const Workers: FC = () => {
  const { t } = useTranslation()
  const [workers, setWorkers] = useState<Worker[]>([])
  const [loading, setLoading] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  const [busy, setBusy] = useState(false)
  const [createOpened, setCreateOpened] = useState(false)
  const [name, setName] = useState('')
  const [query, setQuery] = useState('')
  const [filter, setFilter] = useState<WorkerFilter>('all')
  const [enrollment, setEnrollment] = useState<Enrollment | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<Worker | null>(null)
  const [deleteConfirmation, setDeleteConfirmation] = useState('')

  const loadWorkers = useCallback(async () => {
    try {
      const response = await api.request<Worker[]>({
        path: '/api/admin/workers',
        method: 'GET',
        format: 'json',
      })
      setWorkers(response.data)
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      setLoading(false)
    }
  }, [t])

  useEffect(() => {
    void loadWorkers()
    const timer = window.setInterval(() => void loadWorkers(), 10_000)
    return () => window.clearInterval(timer)
  }, [loadWorkers])

  const installCommands = useMemo<WorkerInstallCommands>(() => {
    const origin =
      import.meta.env.DEV && import.meta.env.VITE_BACKEND_URL
        ? new URL(import.meta.env.VITE_BACKEND_URL).origin
        : window.location.origin

    return {
      linux: workerInstallCommand(origin),
      windows: workerWindowsInstallCommand(origin),
      linuxUninstall: workerUninstallCommand(origin),
      windowsUninstall: workerWindowsUninstallCommand(origin),
    }
  }, [])

  const summary = useMemo(() => {
    const online = workers.filter((worker) => worker.online).length
    const readyWorkers = workers.filter((worker) => worker.online && worker.administrativeState === 'Enabled')

    return {
      online,
      ready: readyWorkers.length,
      activeSlots: readyWorkers.reduce((total, worker) => total + worker.capacity.slots, 0),
    }
  }, [workers])

  const filteredWorkers = useMemo(() => {
    const normalizedQuery = query.trim().toLocaleLowerCase()

    return workers.filter((worker) => {
      const matchesFilter =
        filter === 'all' ||
        (filter === 'online' && worker.online) ||
        (filter === 'offline' && !worker.online) ||
        worker.administrativeState.toLocaleLowerCase() === filter

      if (!matchesFilter) return false
      if (!normalizedQuery) return true

      return [worker.name, worker.id, worker.platformOs, worker.architecture, worker.runtimeKind, worker.runtimeVersion]
        .filter(Boolean)
        .some((value) => value?.toLocaleLowerCase().includes(normalizedQuery))
    })
  }, [filter, query, workers])

  const refreshWorkers = async () => {
    setRefreshing(true)
    await loadWorkers()
    setRefreshing(false)
  }

  const createWorker = async () => {
    if (!name.trim()) return
    setBusy(true)
    try {
      const response = await api.request<CreatedWorker>({
        path: '/api/admin/workers',
        method: 'POST',
        type: ContentType.Json,
        format: 'json',
        body: { name: name.trim() },
      })
      setName('')
      setCreateOpened(false)
      setEnrollment(response.data.enrollment)
      await loadWorkers()
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      setBusy(false)
    }
  }

  const issueToken = async (worker: Worker) => {
    setBusy(true)
    try {
      const response = await api.request<Enrollment>({
        path: `/api/admin/workers/${worker.id}/token`,
        method: 'POST',
        format: 'json',
      })
      setEnrollment(response.data)
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      setBusy(false)
    }
  }

  const updateState = async (worker: Worker, state: WorkerState) => {
    setBusy(true)
    try {
      await api.request<Worker>({
        path: `/api/admin/workers/${worker.id}/state`,
        method: 'PUT',
        type: ContentType.Json,
        format: 'json',
        body: { state },
      })
      await loadWorkers()
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      setBusy(false)
    }
  }

  const openDelete = (worker: Worker) => {
    setDeleteTarget(worker)
    setDeleteConfirmation('')
  }

  const closeDelete = () => {
    if (busy) return
    setDeleteTarget(null)
    setDeleteConfirmation('')
  }

  const deleteWorker = async () => {
    if (!deleteTarget || deleteConfirmation !== deleteTarget.name) return
    setBusy(true)
    try {
      await api.request<void>({
        path: `/api/admin/workers/${deleteTarget.id}`,
        method: 'DELETE',
      })
      showNotification({
        color: 'teal',
        message: t('admin.workers.deleted', 'Deleted retired worker {{name}}', { name: deleteTarget.name }),
        icon: <Icon path={mdiCheck} size={0.8} />,
      })
      setDeleteTarget(null)
      setDeleteConfirmation('')
      await loadWorkers()
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      setBusy(false)
    }
  }

  const stateOptions = [
    { value: 'Enabled', label: t('admin.workers.state.enabled', 'Enabled') },
    { value: 'Draining', label: t('admin.workers.state.draining', 'Draining') },
    { value: 'Disabled', label: t('admin.workers.state.disabled', 'Disabled') },
  ]

  const platformLabel = (worker: Worker): ReactNode => {
    if (!worker.platformOs) {
      return (
        <Text size="sm" c="dimmed">
          {t('admin.workers.not_enrolled', 'Awaiting enrollment')}
        </Text>
      )
    }

    return (
      <Stack gap={1}>
        <Text size="sm" fw={600}>
          {worker.platformOs}/{worker.architecture ?? t('common.label.unknown', 'unknown')}
        </Text>
        <Text size="xs" c="dimmed">
          {worker.runtimeKind ?? t('common.label.unknown', 'Unknown runtime')}
          {worker.runtimeVersion ? ` · ${worker.runtimeVersion}` : ''}
        </Text>
      </Stack>
    )
  }

  const heartbeat = (worker: Worker): ReactNode => {
    if (!worker.heartbeatAt) {
      return (
        <Text size="sm" c="dimmed">
          {t('admin.workers.heartbeat.never', 'Never')}
        </Text>
      )
    }

    const timestamp = dayjs(worker.heartbeatAt)
    return (
      <Text
        component="time"
        dateTime={timestamp.toISOString()}
        title={timestamp.format('YYYY-MM-DD HH:mm:ss Z')}
        size="sm"
        fw={worker.online ? 600 : 400}
      >
        {timestamp.fromNow()}
      </Text>
    )
  }

  const connectionBadge = (worker: Worker): ReactNode => (
    <Badge
      color={worker.online ? 'teal' : 'gray'}
      variant="light"
      className={classes.connectionBadge}
      leftSection={<span className={classes.statusDot} data-online={worker.online || undefined} aria-hidden="true" />}
    >
      {worker.online
        ? t('admin.workers.connection.online', 'Online')
        : t('admin.workers.connection.offline', 'Offline')}
    </Badge>
  )

  const stateSelect = (worker: Worker): ReactNode => (
    <Select
      aria-label={t('admin.workers.state.label', 'Administrative state for {{name}}', { name: worker.name })}
      data={stateOptions}
      value={worker.administrativeState}
      disabled={busy}
      allowDeselect={false}
      onChange={(value) => value && void updateState(worker, value as WorkerState)}
    />
  )

  const deleteDisabledReason = (worker: Worker): string | undefined => {
    if (worker.online) return t('admin.workers.delete.wait_offline', 'Wait for this worker to go offline first')
    if (worker.administrativeState !== 'Disabled') {
      return t('admin.workers.delete.disable_first', 'Set the worker state to Disabled first')
    }
    return undefined
  }

  const actionsMenu = (worker: Worker): ReactNode => {
    const deleteReason = deleteDisabledReason(worker)
    return (
      <Menu position="bottom-end" withinPortal>
        <Menu.Target>
          <Tooltip label={t('admin.workers.actions_for', 'Actions for {{name}}', { name: worker.name })}>
            <ActionIcon
              variant="subtle"
              aria-label={t('admin.workers.actions_for', 'Actions for {{name}}', { name: worker.name })}
              disabled={busy}
            >
              <Icon path={mdiDotsHorizontal} size={0.9} aria-hidden="true" />
            </ActionIcon>
          </Tooltip>
        </Menu.Target>
        <Menu.Dropdown>
          <Menu.Label>{worker.name}</Menu.Label>
          <Menu.Item
            leftSection={<Icon path={mdiKeyChange} size={0.75} aria-hidden="true" />}
            onClick={() => void issueToken(worker)}
          >
            {t('admin.workers.new_token', 'Issue new enrollment token')}
          </Menu.Item>
          <Menu.Divider />
          <Menu.Item
            color="red"
            disabled={Boolean(deleteReason)}
            title={deleteReason}
            leftSection={<Icon path={mdiTrashCanOutline} size={0.75} aria-hidden="true" />}
            onClick={() => openDelete(worker)}
          >
            {t('admin.workers.delete.action', 'Delete worker record')}
          </Menu.Item>
        </Menu.Dropdown>
      </Menu>
    )
  }

  const clearFilters = () => {
    setQuery('')
    setFilter('all')
  }

  return (
    <AdminPage
      isLoading={loading}
      head={
        <>
          <Group gap="xs" wrap="nowrap" className={classes.liveStatus}>
            <span className={classes.liveDot} aria-hidden="true" />
            <Text size="sm" c="dimmed">
              {t('admin.workers.auto_refresh', 'Live inventory · refreshes every 10 seconds')}
            </Text>
          </Group>
          <Group gap="sm" w={{ base: '100%', sm: 'auto' }} grow>
            <Button
              variant="default"
              leftSection={<Icon path={mdiRefresh} size={0.8} aria-hidden="true" />}
              loading={refreshing}
              onClick={() => void refreshWorkers()}
            >
              {t('common.button.refresh', 'Refresh')}
            </Button>
            <Button
              leftSection={<Icon path={mdiPlus} size={0.8} aria-hidden="true" />}
              onClick={() => setCreateOpened(true)}
            >
              {t('admin.workers.add', 'Add worker')}
            </Button>
          </Group>
        </>
      }
    >
      <Stack gap="lg">
        <SimpleGrid cols={{ base: 2, md: 4 }} spacing="md">
          <SummaryMetric
            label={t('admin.workers.summary.total', 'Registered')}
            value={workers.length}
            helper={t('admin.workers.summary.total_helper', 'Trusted worker records')}
            color="blue"
            icon={mdiServerNetwork}
          />
          <SummaryMetric
            label={t('admin.workers.summary.online', 'Online')}
            value={summary.online}
            helper={t('admin.workers.summary.online_helper', 'Heartbeat is current')}
            color="teal"
            icon={mdiCheckCircleOutline}
          />
          <SummaryMetric
            label={t('admin.workers.summary.ready', 'Ready')}
            value={summary.ready}
            helper={t('admin.workers.summary.ready_helper', 'Online and enabled')}
            color="green"
            icon={mdiCheck}
          />
          <SummaryMetric
            label={t('admin.workers.summary.slots', 'Active slots')}
            value={summary.activeSlots}
            helper={t('admin.workers.summary.slots_helper', 'On ready workers')}
            color="violet"
            icon={mdiPackageVariantClosed}
          />
        </SimpleGrid>

        <Paper component="section" withBorder p="md" className={classes.inventory} aria-labelledby="worker-inventory">
          <Group justify="space-between" align="flex-end" gap="md" mb="md" wrap="wrap">
            <Stack gap={2}>
              <Title order={2} size="h4" id="worker-inventory">
                {t('admin.workers.inventory.title', 'Worker inventory')}
              </Title>
              <Text size="sm" c="dimmed">
                {t('admin.workers.inventory.description', 'Search connectivity, runtime, and administrative state.')}
              </Text>
            </Stack>
            {workers.length > 0 && (
              <Text size="sm" c="dimmed" aria-live="polite">
                {t('admin.workers.inventory.showing', 'Showing {{shown}} of {{total}}', {
                  shown: filteredWorkers.length,
                  total: workers.length,
                })}
              </Text>
            )}
          </Group>

          {workers.length > 0 && (
            <Group gap="sm" align="flex-end" mb="lg" className={classes.filters}>
              <TextInput
                type="search"
                label={t('admin.workers.search.label', 'Search workers')}
                placeholder={t('admin.workers.search.placeholder', 'Name, ID, OS, or runtime')}
                leftSection={<Icon path={mdiMagnify} size={0.8} aria-hidden="true" />}
                value={query}
                onChange={(event) => setQuery(event.currentTarget.value)}
                className={classes.search}
              />
              <Select
                label={t('admin.workers.filter.label', 'Status filter')}
                allowDeselect={false}
                value={filter}
                data={[
                  { value: 'all', label: t('admin.workers.filter.all', 'All workers') },
                  { value: 'online', label: t('admin.workers.filter.online', 'Online') },
                  { value: 'offline', label: t('admin.workers.filter.offline', 'Offline') },
                  { value: 'enabled', label: t('admin.workers.filter.enabled', 'Enabled') },
                  { value: 'draining', label: t('admin.workers.filter.draining', 'Draining') },
                  { value: 'disabled', label: t('admin.workers.filter.disabled', 'Disabled') },
                ]}
                onChange={(value) => value && setFilter(value as WorkerFilter)}
                className={classes.filter}
              />
            </Group>
          )}

          {filteredWorkers.length === 0 ? (
            <Empty
              bordered
              title={
                workers.length === 0
                  ? t('admin.workers.empty.title', 'No trusted workers yet')
                  : t('admin.workers.empty.filtered_title', 'No workers match these filters')
              }
              description={
                workers.length === 0
                  ? t(
                      'admin.workers.empty.description',
                      'Add a worker to generate a one-time enrollment token and secure install instructions.'
                    )
                  : t('admin.workers.empty.filtered_description', 'Try another name, platform, or status.')
              }
              action={
                workers.length === 0 ? (
                  <Button leftSection={<Icon path={mdiPlus} size={0.8} />} onClick={() => setCreateOpened(true)}>
                    {t('admin.workers.add', 'Add worker')}
                  </Button>
                ) : (
                  <Button variant="light" onClick={clearFilters}>
                    {t('admin.workers.filter.clear', 'Clear filters')}
                  </Button>
                )
              }
            />
          ) : (
            <>
              <Table.ScrollContainer minWidth={1080} visibleFrom="md">
                <Table verticalSpacing="sm" className={classes.table}>
                  <Table.Caption className="app-sr-only">
                    {t('admin.workers.inventory.caption', 'Trusted worker inventory and controls')}
                  </Table.Caption>
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th scope="col">{t('admin.workers.column.worker', 'Worker')}</Table.Th>
                      <Table.Th scope="col">{t('admin.workers.column.connection', 'Connection')}</Table.Th>
                      <Table.Th scope="col">{t('admin.workers.column.runtime', 'Runtime')}</Table.Th>
                      <Table.Th scope="col">{t('admin.workers.column.capacity', 'Capacity')}</Table.Th>
                      <Table.Th scope="col">{t('admin.workers.column.heartbeat', 'Last heartbeat')}</Table.Th>
                      <Table.Th scope="col">{t('admin.workers.column.state', 'State')}</Table.Th>
                      <Table.Th scope="col">
                        <span className="app-sr-only">{t('common.label.actions', 'Actions')}</span>
                      </Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {filteredWorkers.map((worker) => (
                      <Table.Tr key={worker.id}>
                        <Table.Td>
                          <Stack gap={2} className={classes.identity}>
                            <Text fw={650}>{worker.name}</Text>
                            <Text size="xs" c="dimmed" ff="monospace" truncate title={worker.id}>
                              {worker.id}
                            </Text>
                          </Stack>
                        </Table.Td>
                        <Table.Td>{connectionBadge(worker)}</Table.Td>
                        <Table.Td>{platformLabel(worker)}</Table.Td>
                        <Table.Td>
                          <Stack gap={1}>
                            <Text size="sm" fw={600}>
                              {t('admin.workers.capacity.slots', '{{count}} slots', {
                                count: worker.capacity.slots,
                              })}
                            </Text>
                            <Text size="xs" c="dimmed">
                              {formatCpu(worker.capacity.cpuMillis)} · {formatMemory(worker.capacity.memoryBytes)}
                            </Text>
                          </Stack>
                        </Table.Td>
                        <Table.Td>{heartbeat(worker)}</Table.Td>
                        <Table.Td className={classes.stateCell}>{stateSelect(worker)}</Table.Td>
                        <Table.Td ta="right">{actionsMenu(worker)}</Table.Td>
                      </Table.Tr>
                    ))}
                  </Table.Tbody>
                </Table>
              </Table.ScrollContainer>

              <Stack gap="sm" hiddenFrom="md">
                {filteredWorkers.map((worker) => {
                  const deleteReason = deleteDisabledReason(worker)
                  return (
                    <Paper component="article" key={worker.id} withBorder p="md" className={classes.workerCard}>
                      <Stack gap="md">
                        <Group justify="space-between" align="flex-start" wrap="nowrap">
                          <Stack gap={2} className={classes.identity}>
                            <Text fw={700}>{worker.name}</Text>
                            <Text size="xs" c="dimmed" ff="monospace" truncate title={worker.id}>
                              {worker.id}
                            </Text>
                          </Stack>
                          {connectionBadge(worker)}
                        </Group>

                        <SimpleGrid cols={2} spacing="sm">
                          <Stack gap={2}>
                            <Text size="xs" fw={700} tt="uppercase" c="dimmed">
                              {t('admin.workers.column.runtime', 'Runtime')}
                            </Text>
                            {platformLabel(worker)}
                          </Stack>
                          <Stack gap={2}>
                            <Text size="xs" fw={700} tt="uppercase" c="dimmed">
                              {t('admin.workers.column.heartbeat', 'Last heartbeat')}
                            </Text>
                            {heartbeat(worker)}
                          </Stack>
                          <Stack gap={2}>
                            <Text size="xs" fw={700} tt="uppercase" c="dimmed">
                              {t('admin.workers.column.capacity', 'Capacity')}
                            </Text>
                            <Text size="sm">
                              {t('admin.workers.capacity.slots', '{{count}} slots', {
                                count: worker.capacity.slots,
                              })}
                            </Text>
                            <Text size="xs" c="dimmed">
                              {formatCpu(worker.capacity.cpuMillis)} · {formatMemory(worker.capacity.memoryBytes)}
                            </Text>
                          </Stack>
                        </SimpleGrid>

                        <div>
                          <Text size="xs" fw={700} mb={5}>
                            {t('admin.workers.column.state', 'Administrative state')}
                          </Text>
                          {stateSelect(worker)}
                        </div>

                        <Group grow align="stretch">
                          <Button
                            variant="light"
                            leftSection={<Icon path={mdiKeyChange} size={0.75} />}
                            disabled={busy}
                            onClick={() => void issueToken(worker)}
                          >
                            {t('admin.workers.token.short', 'New token')}
                          </Button>
                          <Tooltip label={deleteReason} disabled={!deleteReason} multiline>
                            <span className={classes.mobileAction}>
                              <Button
                                fullWidth
                                color="red"
                                variant="light"
                                leftSection={<Icon path={mdiTrashCanOutline} size={0.75} />}
                                disabled={busy || Boolean(deleteReason)}
                                onClick={() => openDelete(worker)}
                              >
                                {t('common.button.delete', 'Delete')}
                              </Button>
                            </span>
                          </Tooltip>
                        </Group>
                      </Stack>
                    </Paper>
                  )
                })}
              </Stack>
            </>
          )}
        </Paper>

        <WorkerRetirement commands={installCommands} />
      </Stack>

      <WorkerDialogs
        busy={busy}
        commands={installCommands}
        createOpened={createOpened}
        deleteConfirmation={deleteConfirmation}
        deleteTarget={deleteTarget}
        enrollment={enrollment}
        name={name}
        onCloseCreate={() => !busy && setCreateOpened(false)}
        onCloseDelete={closeDelete}
        onCloseEnrollment={() => setEnrollment(null)}
        onCreate={() => void createWorker()}
        onDelete={() => void deleteWorker()}
        onDeleteConfirmationChange={setDeleteConfirmation}
        onNameChange={setName}
      />
    </AdminPage>
  )
}

export default Workers
