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
  mdiCheck,
  mdiChevronTripleRight,
  mdiConsole,
  mdiPackageVariantClosedRemove,
  mdiPuzzleOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useEffect, useState } from 'react'
import { Trans, useTranslation } from 'react-i18next'
import { ActionIconWithConfirm } from '@Components/ActionIconWithConfirm'
import { AdminPage } from '@Components/admin/AdminPage'
import { ContainerExecModal } from '@Components/admin/ContainerExecModal'
import { containerOwnerLabel, hasContainerProxy } from '@Utils/ContainerInstance'
import { useLanguage } from '@Utils/I18n'
import { showErrorMsg } from '@Utils/Shared'
import { HunamizeSize, useChallengeCategoryLabelMap, getProxyUrl } from '@Utils/Shared'
import api, { ChallengeModel, ChallengeCategory, ContainerInstanceModel, TeamModel } from '@Api'
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

const InstanceStatsCells: FC<{ instanceGuid?: string; live: boolean }> = ({ instanceGuid, live }) => {
  const { data, isLoading } = api.admin.useAdminGetInstanceStats(
    instanceGuid ?? '',
    live ? { refreshInterval: 5000, revalidateOnFocus: false } : { revalidateIfStale: false, refreshInterval: 0 },
    !!instanceGuid && live
  )

  const placeholder = (
    <Text size="xs" c="dimmed" ta="center">
      —
    </Text>
  )

  if (!instanceGuid)
    return (
      <>
        {placeholder}
        {placeholder}
        {placeholder}
      </>
    )
  if (!data && isLoading)
    return (
      <>
        {placeholder}
        {placeholder}
        {placeholder}
      </>
    )
  if (!data)
    return (
      <>
        {placeholder}
        {placeholder}
        {placeholder}
      </>
    )

  const memPct = data.memoryLimitBytes > 0 ? Math.min(100, (data.memoryUsedBytes / data.memoryLimitBytes) * 100) : 0

  return (
    <>
      <Table.Td>
        <Stack gap={2} miw="5rem">
          <Text size="xs" ff="monospace">
            {data.cpuPercent.toFixed(1)}%
          </Text>
          <Progress value={Math.min(100, data.cpuPercent)} color={barColor(data.cpuPercent)} size="xs" />
        </Stack>
      </Table.Td>
      <Table.Td>
        <Stack gap={2} miw="7rem">
          <Text size="xs" ff="monospace">
            {HunamizeSize(data.memoryUsedBytes)} / {HunamizeSize(data.memoryLimitBytes)}
          </Text>
          <Progress value={memPct} color={barColor(memPct)} size="xs" />
        </Stack>
      </Table.Td>
      <Table.Td>
        <Stack gap={2}>
          <Text size="xs" ff="monospace" c="green">
            ↓ {HunamizeSize(data.netRxBytes)}
          </Text>
          <Text size="xs" ff="monospace" c="blue">
            ↑ {HunamizeSize(data.netTxBytes)}
          </Text>
        </Stack>
      </Table.Td>
    </>
  )
}

const Instances: FC = () => {
  const { data: instances, mutate } = api.admin.useAdminInstances({
    refreshInterval: 30 * 1000, // refresh every 30 seconds
    revalidateOnFocus: false,
  })

  const [teams, setTeams] = useState<TeamModel[]>()
  const [challenge, setChallenge] = useState<ChallengeModel[]>()
  const [disabled, setDisabled] = useState(false)
  const clipBoard = useClipboard()
  const challengeCategoryLabelMap = useChallengeCategoryLabelMap()

  const { t } = useTranslation()
  const { locale } = useLanguage()

  useEffect(() => {
    if (instances) {
      // Shared StaticContainers have no owning team (and the row carries team=null), so filter
      // teamless rows before deduping — otherwise `instance.team!.id` throws on the null and
      // crashes the whole page. (Matches the optional-chaining used for filtering/rendering below.)
      const teams = [
        ...new Map(
          instances.data.filter((i) => i.team).map((instance) => [instance.team!.id, instance.team!])
        ).values(),
      ]
      setTeams(teams)

      const challenges = [
        ...new Map(
          instances.data.filter((i) => i.challenge).map((instance) => [instance.challenge!.id, instance.challenge!])
        ).values(),
      ]
      setChallenge(challenges)
    }
  }, [instances])

  const [selectedTeamId, setSelectedTeamId] = useState<string | null>(null)
  const [selectedChallengeId, setSelectedChallengeId] = useState<string | null>(null)
  const [liveStats, setLiveStats] = useState(true)
  const [execTarget, setExecTarget] = useState<{ guid: string; title: string } | null>(null)

  const [filteredInstances, setFilteredInstances] = useState(instances?.data)

  useEffect(() => {
    if (!instances) return

    let filtered = instances.data

    if (selectedTeamId) {
      filtered = filtered.filter((instance) => instance.team?.id === Number(selectedTeamId))
    }

    if (selectedChallengeId) {
      filtered = filtered.filter((instance) => instance.challenge?.id === Number(selectedChallengeId))
    }

    setFilteredInstances(filtered)
  }, [instances, selectedTeamId, selectedChallengeId])

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

      if (instances) {
        mutate({
          total: (instances.total ?? instances.length) - 1,
          length: instances.length - 1,
          data: instances.data.filter((instance) => instance.containerGuid !== instanceGuid),
        })
      }
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
      isLoading={!instances || !teams || !challenge}
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
              data={teams?.map((team) => ({ value: String(team.id), label: team.name, ...team }) as ComboboxItem) ?? []}
            />
            <Select
              w={{ base: '100%', sm: 'calc(50% - var(--mantine-spacing-md) / 2)' }}
              aria-label={t('admin.label.instances.challenge_filter', 'Filter instances by challenge')}
              searchable
              clearable
              placeholder={t('admin.placeholder.instances.challenges.select')}
              onChange={(id) => setSelectedChallengeId(id)}
              leftSection={<Icon path={mdiPuzzleOutline} size={1} />}
              nothingFoundMessage={t('admin.placeholder.instances.challenges.not_found')}
              renderOption={SelectChallengeItem}
              data={
                challenge?.map(
                  (challenge) =>
                    ({
                      value: String(challenge.id),
                      label: challenge.title,
                      ...challenge,
                    }) as ComboboxItem
                ) ?? []
              }
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
              <Trans i18nKey="admin.content.instances.stats" values={{ count: instances?.length }}>
                _<Code>_</Code>_
              </Trans>
            </Text>
          </Group>
        </>
      }
    >
      <Paper shadow="md" p="xs" w="100%">
        <ScrollArea offsetScrollbars scrollbarSize={4} h="calc(100vh - 205px)">
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
                <Table.Th scope="col" aria-label={t('common.label.action', 'Actions')} />
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>
              {filteredInstances &&
                filteredInstances.map((inst) => {
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
                            value={
                              inst.challenge?.title ?? t('admin.label.instances.challenge_unassigned', 'Unassigned')
                            }
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
                      <InstanceStatsCells instanceGuid={inst.containerGuid} live={liveStats} />
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
