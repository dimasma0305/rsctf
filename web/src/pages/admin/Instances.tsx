import {
  ActionIcon,
  Badge,
  Box,
  Code,
  ComboboxItem,
  Group,
  Input,
  Paper,
  Progress,
  ScrollArea,
  Select,
  SelectProps,
  Stack,
  Switch,
  Table,
  Text,
  Tooltip,
  useMantineTheme,
} from '@mantine/core'
import { useClipboard } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import {
  mdiAccountGroupOutline,
  mdiArrowLeftBold,
  mdiArrowRightBold,
  mdiCheck,
  mdiChevronTripleRight,
  mdiConsole,
  mdiPackageVariantClosedRemove,
  mdiPuzzleOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useEffect, useMemo, useState } from 'react'
import { Trans, useTranslation } from 'react-i18next'
import { ActionIconWithConfirm } from '@Components/ActionIconWithConfirm'
import { AdminPage } from '@Components/admin/AdminPage'
import { ContainerExecModal } from '@Components/admin/ContainerExecModal'
import { useAdminInstancePollingConfig } from '@Utils/AdminInstancePolling'
import { containerOwnerLabel, hasContainerProxy } from '@Utils/ContainerInstance'
import { useLanguage } from '@Utils/I18n'
import { showErrorMsg } from '@Utils/Shared'
import { HunamizeSize, useChallengeCategoryLabelMap, getProxyUrl } from '@Utils/Shared'
import api, {
  ChallengeModel,
  ChallengeCategory,
  ContainerInstanceModel,
  ContainerRuntimeAvailability,
  ContainerRuntimeStatsModel,
  TeamModel,
} from '@Api'
import classes from '@Styles/Instances.module.css'
import misc from '@Styles/Misc.module.css'
import tableClasses from '@Styles/Table.module.css'

type SelectTeamItemProps = TeamModel & ComboboxItem
type SelectChallengeItemProps = ChallengeModel & ComboboxItem

const SelectTeamItem: SelectProps['renderOption'] = ({ option }) => {
  const { name, id, ...others } = option as SelectTeamItemProps

  return (
    <Group {...others} gap={0} wrap="nowrap">
      <Text fw={500} size="sm" lineClamp={1} className={misc.wordBreakAll}>
        <Text span c="dimmed">
          {`#${id} `}
        </Text>
        {name}
      </Text>
    </Group>
  )
}

const SelectChallengeItem: SelectProps['renderOption'] = ({ option }) => {
  const { title, id, category } = option as SelectChallengeItemProps
  const challengeCategoryLabelMap = useChallengeCategoryLabelMap()
  const cateData = challengeCategoryLabelMap.get(category ?? ChallengeCategory.Misc)!
  const theme = useMantineTheme()

  return (
    <Group wrap="nowrap" gap="sm">
      <Icon color={theme.colors[cateData.color][4]} path={cateData.icon} size={1} />
      <Text fw={500} size="sm" lineClamp={1} className={misc.wordBreakAll}>
        <Text span c="dimmed">
          {`#${id} `}
        </Text>
        {title}
      </Text>
    </Group>
  )
}

const barColor = (pct: number) => (pct >= 85 ? 'red' : pct >= 60 ? 'yellow' : 'teal')

const UnavailableStatsValue: FC<{ label: string }> = ({ label }) => (
  <Text size="xs" c="dimmed" ta="center" title={label}>
    <span aria-hidden="true">—</span>
    <span className="app-sr-only">{label}</span>
  </Text>
)

const UnavailableStatsCell: FC<{ label: string }> = ({ label }) => (
  <Table.Td>
    <UnavailableStatsValue label={label} />
  </Table.Td>
)

const InstanceStatsCells: FC<{ stats?: ContainerRuntimeStatsModel; live: boolean }> = ({ stats, live }) => {
  const { t } = useTranslation()
  const unavailableLabel = live
    ? t('admin.label.instances.stats_unavailable', 'Runtime metric unavailable')
    : t('admin.label.instances.stats_disabled', 'Live statistics disabled')

  if (!live || !stats || stats.availability === ContainerRuntimeAvailability.Unavailable) {
    return (
      <>
        <UnavailableStatsCell label={unavailableLabel} />
        <UnavailableStatsCell label={unavailableLabel} />
        <UnavailableStatsCell label={unavailableLabel} />
      </>
    )
  }

  const cpu = typeof stats.cpuPercent === 'number' && Number.isFinite(stats.cpuPercent) ? stats.cpuPercent : null
  const memoryUsed =
    typeof stats.memoryUsedBytes === 'number' && Number.isFinite(stats.memoryUsedBytes) ? stats.memoryUsedBytes : null
  const memoryLimit =
    typeof stats.memoryLimitBytes === 'number' && stats.memoryLimitBytes > 0 ? stats.memoryLimitBytes : null
  const memPct = memoryUsed !== null && memoryLimit !== null ? Math.min(100, (memoryUsed / memoryLimit) * 100) : null
  const rx = typeof stats.netRxBytes === 'number' && Number.isFinite(stats.netRxBytes) ? stats.netRxBytes : null
  const tx = typeof stats.netTxBytes === 'number' && Number.isFinite(stats.netTxBytes) ? stats.netTxBytes : null

  return (
    <>
      <Table.Td>
        {cpu === null ? (
          <UnavailableStatsValue label={unavailableLabel} />
        ) : (
          <Stack gap={2} miw="5rem">
            <Text size="xs" ff="monospace">
              {cpu.toFixed(1)}%
            </Text>
            <Progress value={Math.min(100, cpu)} color={barColor(cpu)} size="xs" />
          </Stack>
        )}
      </Table.Td>
      <Table.Td>
        {memoryUsed === null ? (
          <UnavailableStatsValue label={unavailableLabel} />
        ) : (
          <Stack gap={2} miw="7rem">
            <Text size="xs" ff="monospace">
              {HunamizeSize(memoryUsed)}
              {memoryLimit !== null ? ` / ${HunamizeSize(memoryLimit)}` : ''}
            </Text>
            {memPct === null ? (
              <Text size="xs" c="dimmed">
                {t('admin.label.instances.limit_unavailable', 'Limit unavailable')}
              </Text>
            ) : (
              <Progress value={memPct} color={barColor(memPct)} size="xs" />
            )}
          </Stack>
        )}
      </Table.Td>
      <Table.Td>
        {rx === null || tx === null ? (
          <UnavailableStatsValue label={unavailableLabel} />
        ) : (
          <Stack gap={2}>
            <Text size="xs" ff="monospace" c="green">
              ↓ {HunamizeSize(rx)}
            </Text>
            <Text size="xs" ff="monospace" c="blue">
              ↑ {HunamizeSize(tx)}
            </Text>
          </Stack>
        )}
      </Table.Td>
    </>
  )
}

const ITEM_COUNT_PER_PAGE = 25

const Instances: FC = () => {
  const [page, setPage] = useState(1)
  const [liveStats, setLiveStats] = useState(true)
  const pollingConfig = useAdminInstancePollingConfig(liveStats)
  const instanceQuery = useMemo(
    () => ({
      count: ITEM_COUNT_PER_PAGE,
      skip: (page - 1) * ITEM_COUNT_PER_PAGE,
      includeRuntimeStats: liveStats,
    }),
    [liveStats, page]
  )
  const { data: instances, mutate } = api.admin.useAdminInstancesPage(instanceQuery, pollingConfig)

  const [disabled, setDisabled] = useState(false)
  const clipBoard = useClipboard()
  const challengeCategoryLabelMap = useChallengeCategoryLabelMap()

  const { t } = useTranslation()
  const { locale } = useLanguage()

  const teams = useMemo(
    () => [
      ...new Map(
        (instances?.data ?? [])
          .filter((instance): instance is ContainerInstanceModel & { team: TeamModel } => !!instance.team)
          .map((instance) => [instance.team.id, instance.team])
      ).values(),
    ],
    [instances]
  )
  const challenges = useMemo(
    () => [
      ...new Map(
        (instances?.data ?? [])
          .filter(
            (instance): instance is ContainerInstanceModel & { challenge: ChallengeModel } => !!instance.challenge
          )
          .map((instance) => [instance.challenge.id, instance.challenge])
      ).values(),
    ],
    [instances]
  )

  const [selectedTeamId, setSelectedTeamId] = useState<string | null>(null)
  const [selectedChallengeId, setSelectedChallengeId] = useState<string | null>(null)
  const [execTarget, setExecTarget] = useState<{ guid: string; title: string } | null>(null)

  const filteredInstances = useMemo(() => {
    let filtered = instances?.data ?? []

    if (selectedTeamId) {
      filtered = filtered.filter((instance) => instance.team?.id === Number(selectedTeamId))
    }

    if (selectedChallengeId) {
      filtered = filtered.filter((instance) => instance.challenge?.id === Number(selectedChallengeId))
    }

    return filtered
  }, [instances, selectedTeamId, selectedChallengeId])

  const total = instances?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / ITEM_COUNT_PER_PAGE))
  useEffect(() => {
    if (page > totalPages) setPage(totalPages)
  }, [page, totalPages])

  const onDelete = async (instanceGuid?: string) => {
    if (!instanceGuid) return

    try {
      setDisabled(true)
      await api.admin.adminDestroyInstance(instanceGuid)

      showNotification({
        color: 'teal',
        message: t('admin.notification.instances.destroyed'),
        icon: <Icon path={mdiCheck} size={1} />,
      })

      if (instances?.data.length === 1 && page > 1) setPage(page - 1)
      else await mutate()
    } catch (e: any) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  const copyContainerUrl = (instance: ContainerInstanceModel) => () => {
    if (!hasContainerProxy(instance)) return

    clipBoard.copy(getProxyUrl(instance.containerGuid))
    showNotification({
      color: 'teal',
      title: t('admin.notification.instances.url_copied.title'),
      message: t('admin.notification.instances.url_copied.message'),
      icon: <Icon path={mdiCheck} size={1} />,
    })
  }

  const copyEntry = (ip?: string | null, port?: number | null) => () => {
    clipBoard.copy(`${ip ?? ''}:${port ?? ''}`)
    showNotification({
      color: 'teal',
      message: t('admin.notification.instances.entry_copied'),
      icon: <Icon path={mdiCheck} size={1} />,
    })
  }

  return (
    <AdminPage
      isLoading={!instances}
      head={
        <>
          <Group w={{ base: '100%', md: '60%' }} justify="left" gap="md" wrap="wrap">
            <Select
              w={{ base: '100%', sm: 'calc(50% - var(--mantine-spacing-md) / 2)' }}
              aria-label={t('admin.label.instances.team_filter', 'Filter instances by team')}
              searchable
              clearable
              placeholder={t('admin.placeholder.instances.teams.select')}
              value={selectedTeamId}
              onChange={(id) => setSelectedTeamId(id)}
              leftSection={<Icon path={mdiAccountGroupOutline} size={1} />}
              nothingFoundMessage={t('admin.placeholder.instances.teams.not_found')}
              renderOption={SelectTeamItem}
              data={teams.map((team) => ({ value: String(team.id), label: team.name, ...team }) as ComboboxItem)}
            />
            <Select
              w={{ base: '100%', sm: 'calc(50% - var(--mantine-spacing-md) / 2)' }}
              aria-label={t('admin.label.instances.challenge_filter', 'Filter instances by challenge')}
              searchable
              clearable
              placeholder={t('admin.placeholder.instances.challenges.select')}
              value={selectedChallengeId}
              onChange={(id) => setSelectedChallengeId(id)}
              leftSection={<Icon path={mdiPuzzleOutline} size={1} />}
              nothingFoundMessage={t('admin.placeholder.instances.challenges.not_found')}
              renderOption={SelectChallengeItem}
              data={challenges.map(
                (challenge) =>
                  ({
                    value: String(challenge.id),
                    label: challenge.title,
                    ...challenge,
                  }) as ComboboxItem
              )}
            />
          </Group>

          <Group justify="right" gap="md" wrap="wrap">
            <Switch
              size="xs"
              label={t('admin.label.instances.live_stats')}
              checked={liveStats}
              onChange={(e) => setLiveStats(e.currentTarget.checked)}
            />
            <Text fw="bold" size="sm">
              <Trans i18nKey="admin.content.instances.stats" values={{ count: total }}>
                _<Code>_</Code>_
              </Trans>
            </Text>
            <Group role="group" gap="xs" wrap="nowrap" aria-label={t('common.pagination.label', 'Pagination')}>
              <ActionIcon
                size={44}
                disabled={page <= 1}
                aria-label={t('common.pagination.previous', 'Previous page')}
                onClick={() => setPage((current) => Math.max(1, current - 1))}
              >
                <Icon path={mdiArrowLeftBold} size={1} />
              </ActionIcon>
              <Text fw="bold" size="sm" aria-live="polite">
                {t('common.pagination.page_of', {
                  defaultValue: 'Page {{page}} of {{total}}',
                  page,
                  total: totalPages,
                })}
              </Text>
              <ActionIcon
                size={44}
                disabled={page >= totalPages}
                aria-label={t('common.pagination.next', 'Next page')}
                onClick={() => setPage((current) => Math.min(totalPages, current + 1))}
              >
                <Icon path={mdiArrowRightBold} size={1} />
              </ActionIcon>
            </Group>
          </Group>
        </>
      }
    >
      <Paper shadow="md" p="xs" w="100%">
        <ScrollArea
          offsetScrollbars
          scrollbarSize={8}
          h="calc(100vh - 205px)"
          viewportProps={{
            tabIndex: 0,
            'aria-label': t('admin.content.instances.scroll_label', 'Scrollable active challenge instances'),
          }}
        >
          <Table className={tableClasses.table}>
            <Table.Caption>{t('admin.content.instances.table_caption', 'Active challenge instances')}</Table.Caption>
            <Table.Thead>
              <Table.Tr>
                <Table.Th scope="col">{t('common.label.team')}</Table.Th>
                <Table.Th scope="col">{t('common.label.challenge')}</Table.Th>
                <Table.Th scope="col">{t('admin.label.instances.life_cycle')}</Table.Th>
                <Table.Th scope="col">{t('admin.label.instances.cpu')}</Table.Th>
                <Table.Th scope="col">{t('admin.label.instances.memory')}</Table.Th>
                <Table.Th scope="col">{t('admin.label.instances.network')}</Table.Th>
                <Table.Th scope="col">{t('admin.label.instances.container_id')}</Table.Th>
                <Table.Th scope="col">{t('admin.label.instances.entry')}</Table.Th>
                <Table.Th scope="col">
                  <span className="app-sr-only">{t('common.label.action', 'Actions')}</span>
                </Table.Th>
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>
              {filteredInstances.map((inst) => {
                const color = challengeCategoryLabelMap.get(inst.challenge?.category ?? ChallengeCategory.Misc)!.color
                const ownerLabel = containerOwnerLabel(inst, {
                  shared: t('admin.label.instances.owner.shared', 'Shared (all teams)'),
                  adminTest: t('admin.label.instances.owner.admin_test', 'Admin test'),
                  exercise: t('admin.label.instances.owner.exercise', 'Exercise'),
                  unassigned: t('admin.label.instances.owner.unassigned', 'Unassigned'),
                })
                return (
                  <Table.Tr key={inst.containerGuid}>
                    <Table.Td>
                      <Box w="100%" h="100%">
                        <Input
                          variant="unstyled"
                          value={ownerLabel}
                          aria-label={t('common.label.team')}
                          readOnly
                          classNames={classes}
                        />
                      </Box>
                    </Table.Td>
                    <Table.Td>
                      <Box w="100%" h="100%">
                        <Input
                          variant="unstyled"
                          value={inst.challenge?.title ?? t('admin.label.instances.challenge_unassigned', 'Unassigned')}
                          aria-label={t('common.label.challenge')}
                          readOnly
                          classNames={classes}
                        />
                      </Box>
                    </Table.Td>
                    <Table.Td>
                      <Group wrap="nowrap" gap="xs">
                        <Badge size="xs" color={color} variant="dot">
                          {dayjs(inst.startedAt).locale(locale).format('SL HH:mm')}
                        </Badge>
                        <Icon path={mdiChevronTripleRight} size={1} />
                        <Badge size="xs" color={color} variant="dot">
                          {dayjs(inst.expectStopAt).locale(locale).format('SL HH:mm')}
                        </Badge>
                      </Group>
                    </Table.Td>
                    <InstanceStatsCells stats={inst.runtimeStats} live={liveStats} />
                    <Table.Td>
                      <Text size="sm" ff="monospace" lineClamp={1}>
                        {hasContainerProxy(inst) ? (
                          <Tooltip
                            label={t('admin.label.instances.copy_proxy_url', 'Copy proxy URL')}
                            withArrow
                            position="left"
                          >
                            <Text
                              size="sm"
                              ff="monospace"
                              bg="transparent"
                              fz="sm"
                              role="button"
                              tabIndex={0}
                              aria-label={t('admin.notification.instances.url_copied.title')}
                              className={tableClasses.clickable}
                              onClick={copyContainerUrl(inst)}
                              onKeyDown={(e) => {
                                if (e.key === 'Enter' || e.key === ' ') {
                                  e.preventDefault()
                                  copyContainerUrl(inst)()
                                }
                              }}
                            >
                              {inst.containerGuid}
                            </Text>
                          </Tooltip>
                        ) : (
                          <Text size="sm" ff="monospace" bg="transparent" fz="sm">
                            {inst.containerGuid}
                          </Text>
                        )}
                      </Text>
                    </Table.Td>
                    <Table.Td>
                      <Tooltip label={t('common.button.copy')} withArrow position="left">
                        <Text
                          size="sm"
                          c="dimmed"
                          ff="monospace"
                          bg="transparent"
                          fz="sm"
                          role="button"
                          tabIndex={0}
                          aria-label={t('admin.notification.instances.entry_copied')}
                          className={tableClasses.clickable}
                          onClick={copyEntry(inst.ip, inst.port)}
                          onKeyDown={(e) => {
                            if (e.key === 'Enter' || e.key === ' ') {
                              e.preventDefault()
                              copyEntry(inst.ip, inst.port)()
                            }
                          }}
                        >
                          {`${inst.ip}:`}
                          <Text span fw="bold">
                            {inst.port}
                          </Text>
                        </Text>
                      </Tooltip>
                    </Table.Td>
                    <Table.Td align="right">
                      <Group wrap="nowrap" gap="xs" justify="right">
                        <Tooltip label={t('admin.button.exec.open')} withArrow position="left">
                          <ActionIcon
                            variant="subtle"
                            disabled={!inst.containerGuid}
                            aria-label={t('admin.button.exec.open')}
                            onClick={() =>
                              inst.containerGuid &&
                              setExecTarget({
                                guid: inst.containerGuid,
                                title: `${ownerLabel} - ${inst.challenge?.title ?? ''}`,
                              })
                            }
                          >
                            <Icon path={mdiConsole} size={1} />
                          </ActionIcon>
                        </Tooltip>
                        <ActionIconWithConfirm
                          iconPath={mdiPackageVariantClosedRemove}
                          color="alert"
                          message={t('admin.content.instances.destroy', {
                            name: inst.containerGuid?.slice(0, 8),
                          })}
                          disabled={disabled}
                          onClick={() => onDelete(inst.containerGuid)}
                        />
                      </Group>
                    </Table.Td>
                  </Table.Tr>
                )
              })}
            </Table.Tbody>
          </Table>
        </ScrollArea>
        <Text size="xs" c="dimmed">
          {t('admin.content.instances.note')}
        </Text>
      </Paper>
      <ContainerExecModal
        containerGuid={execTarget?.guid ?? null}
        containerTitle={execTarget?.title}
        opened={execTarget != null}
        onClose={() => setExecTarget(null)}
      />
    </AdminPage>
  )
}

export default Instances
