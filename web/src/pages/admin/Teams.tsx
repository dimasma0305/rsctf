import {
  ActionIcon,
  Avatar,
  Badge,
  Box,
  Card,
  Code,
  Divider,
  Group,
  Paper,
  ScrollArea,
  Stack,
  Table,
  Text,
  TextInput,
  Tooltip,
} from '@mantine/core'
import { useInputState } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import {
  mdiAccountGroupOutline,
  mdiArrowLeftBold,
  mdiArrowRightBold,
  mdiCheck,
  mdiDeleteOutline,
  mdiLockOpenVariantOutline,
  mdiLockOutline,
  mdiMagnify,
  mdiPencilOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useRef, useState } from 'react'
import { Trans, useTranslation } from 'react-i18next'
import { ActionIconWithConfirm } from '@Components/ActionIconWithConfirm'
import { ScrollingText } from '@Components/ScrollingText'
import { AdminPage } from '@Components/admin/AdminPage'
import { TeamEditModal } from '@Components/admin/TeamEditModal'
import { showErrorMsg } from '@Utils/Shared'
import { useArrayResponse } from '@Hooks/useArrayResponse'
import api, { TeamInfoModel } from '@Api'
import tableClasses from '@Styles/Table.module.css'
import mobileClasses from './AdminMobileList.module.css'

const ITEM_COUNT_PER_PAGE = 30

const Teams: FC = () => {
  const [page, setPage] = useState(1)
  const [update, setUpdate] = useState(new Date())
  const { data: teams, total, setData: setTeams, updateData: updateTeams } = useArrayResponse<TeamInfoModel>()
  const [hint, setHint] = useInputState('')
  const [searching, setSearching] = useState(false)
  const [disabled, setDisabled] = useState(false)
  const [current, setCurrent] = useState(0)
  const [isEditModalOpen, setIsEditModalOpen] = useState(false)
  const [activeTeam, setActiveTeam] = useState<TeamInfoModel>({})

  const { t } = useTranslation()
  const viewport = useRef<HTMLDivElement>(null)

  useEffect(() => {
    viewport.current?.scrollTo({ top: 0, behavior: 'smooth' })
  }, [page, viewport])

  useEffect(() => {
    const fetchData = async () => {
      try {
        const res = await api.admin.adminTeams({
          count: ITEM_COUNT_PER_PAGE,
          skip: (page - 1) * ITEM_COUNT_PER_PAGE,
        })

        setTeams(res.data)
        setCurrent((page - 1) * ITEM_COUNT_PER_PAGE + res.data.length)
      } catch (e) {
        showErrorMsg(e, t)
      }
    }

    fetchData()
  }, [page, update])

  const onSearch = async () => {
    try {
      if (!hint) {
        const res = await api.admin.adminTeams({
          count: ITEM_COUNT_PER_PAGE,
          skip: (page - 1) * ITEM_COUNT_PER_PAGE,
        })

        setTeams(res.data)
        setCurrent((page - 1) * ITEM_COUNT_PER_PAGE + res.data.length)
      } else {
        setSearching(true)

        const res = await api.admin.adminSearchTeams({ hint })
        setTeams(res.data)
        setCurrent(res.data.length)
      }
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setSearching(false)
    }
  }

  const onDelete = async (team: TeamInfoModel) => {
    try {
      if (!team.id) return
      setDisabled(true)

      await api.admin.adminDeleteTeam(team.id)

      showNotification({
        message: t('admin.notification.teams.deleted', {
          name: team.name,
        }),
        color: 'teal',
        icon: <Icon path={mdiCheck} size={1} />,
      })
      if (teams) updateTeams(teams.filter((x) => x.id !== team.id))
      setCurrent(current - 1)
      setUpdate(new Date())
    } catch (e: any) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  const onToggleLock = async (team: TeamInfoModel) => {
    try {
      if (!team.id) return
      setDisabled(true)

      await api.admin.adminUpdateTeam(team.id!, {
        locked: !team.locked,
      })

      showNotification({
        color: 'teal',
        message: t('team.notification.updated'),
        icon: <Icon path={mdiCheck} size={1} />,
      })

      updateTeams(
        [{ ...team, locked: !team.locked }, ...(teams?.filter((n) => n.id !== team.id) ?? [])].sort((a, b) =>
          a.id! < b.id! ? -1 : 1
        )
      )
      setUpdate(new Date())
    } catch (e: any) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  return (
    <AdminPage
      isLoading={searching || !teams}
      head={
        <>
          <TextInput
            w={{ base: '100%', sm: '36%' }}
            aria-label={t('admin.placeholder.teams.search')}
            leftSection={<Icon path={mdiMagnify} size={1} />}
            placeholder={t('admin.placeholder.teams.search')}
            value={hint}
            onChange={setHint}
            onKeyDown={(e) => {
              if (!searching && e.key === 'Enter') onSearch()
            }}
            rightSection={<Icon path={mdiAccountGroupOutline} size={1} />}
          />
          <Group justify="right" wrap="wrap">
            <Text fw="bold" size="sm">
              <Trans
                i18nKey="admin.content.teams.stats"
                values={{
                  current,
                  total,
                }}
              >
                _<Code>_</Code>_
              </Trans>
            </Text>
            <ActionIcon
              size="lg"
              disabled={page <= 1}
              aria-label={t('common.pagination.previous', 'Previous page')}
              onClick={() => setPage(page - 1)}
            >
              <Icon path={mdiArrowLeftBold} size={1} />
            </ActionIcon>
            <Text fw="bold" size="sm">
              {page}
            </Text>
            <ActionIcon
              size="lg"
              disabled={page * ITEM_COUNT_PER_PAGE >= total}
              aria-label={t('common.pagination.next', 'Next page')}
              onClick={() => setPage(page + 1)}
            >
              <Icon path={mdiArrowRightBold} size={1} />
            </ActionIcon>
          </Group>
        </>
      }
    >
      <Paper shadow="md" p="md" w="100%">
        <Box visibleFrom="sm">
          <ScrollArea viewportRef={viewport} offsetScrollbars scrollbarSize={4} h="calc(100vh - 190px)">
            <Table className={tableClasses.table}>
              <Table.Caption>{t('admin.content.teams.table_caption', 'Registered teams')}</Table.Caption>
              <Table.Thead>
                <Table.Tr>
                  <Table.Th scope="col" w="35vw" miw="400px">
                    {t('common.label.team')}
                  </Table.Th>
                  <Table.Th scope="col">{t('admin.label.teams.members')}</Table.Th>
                  <Table.Th scope="col">{t('admin.label.teams.bio')}</Table.Th>
                  <Table.Th scope="col" aria-label={t('common.label.action', 'Actions')} />
                </Table.Tr>
              </Table.Thead>
              <Table.Tbody>
                {teams &&
                  teams.map((team) => {
                    const members = team.members?.sort((a, b) => (a.captain ? (b.captain ? 0 : -1) : 1))

                    return (
                      <Table.Tr key={team.id}>
                        <Table.Td>
                          <Group justify="space-between" gap={0} wrap="nowrap">
                            <Group justify="left" wrap="nowrap" w="calc(100% - 7rem)">
                              <Avatar imageProps={{ loading: 'lazy' }} alt="avatar" src={team.avatar} radius="xl">
                                {team.name?.slice(0, 1)}
                              </Avatar>
                              <ScrollingText text={team.name ?? 'team'} fw="bold" maw={180} />
                            </Group>
                            <Badge size="md" color={team.locked ? 'yellow' : 'gray'}>
                              {team.locked ? t('admin.content.teams.locked') : t('admin.content.teams.unlocked')}
                            </Badge>
                          </Group>
                        </Table.Td>
                        <Table.Td>
                          <Tooltip.Group openDelay={300} closeDelay={100}>
                            <Avatar.Group spacing="md">
                              {members &&
                                members.slice(0, 8).map((m) => (
                                  <Tooltip key={m.id} label={m.userName} withArrow>
                                    <Avatar imageProps={{ loading: 'lazy' }} alt="avatar" radius="xl" src={m.avatar}>
                                      {m.userName?.slice(0, 1) ?? 'U'}
                                    </Avatar>
                                  </Tooltip>
                                ))}
                              {members && members.length > 8 && (
                                <Tooltip
                                  label={
                                    <Text>
                                      {members
                                        .slice(8)
                                        .map((o) => o.userName)
                                        .join(',')}
                                    </Text>
                                  }
                                  withArrow
                                >
                                  <Avatar imageProps={{ loading: 'lazy' }} alt="avatar" radius="xl">
                                    +{members.length - 8}
                                  </Avatar>
                                </Tooltip>
                              )}
                            </Avatar.Group>
                          </Tooltip.Group>
                        </Table.Td>
                        <Table.Td>
                          <ScrollingText text={team.bio ?? t('team.placeholder.bio')} size="sm" maw={140} />
                        </Table.Td>
                        <Table.Td align="right">
                          <Group wrap="nowrap" gap="sm" justify="right">
                            <ActionIcon
                              color="blue"
                              aria-label={t('admin.button.teams.edit', 'Edit {{name}}', { name: team.name })}
                              onClick={() => {
                                setActiveTeam(team)
                                setIsEditModalOpen(true)
                              }}
                            >
                              <Icon path={mdiPencilOutline} size={1} />
                            </ActionIcon>

                            <ActionIconWithConfirm
                              iconPath={team.locked ? mdiLockOpenVariantOutline : mdiLockOutline}
                              color={team.locked ? 'gray' : 'yellow'}
                              message={t('admin.content.teams.lock', {
                                name: team.name,
                                action: team.locked
                                  ? t('admin.button.teams.do_unlock')
                                  : t('admin.button.teams.do_lock'),
                              })}
                              disabled={disabled}
                              onClick={() => onToggleLock(team)}
                            />

                            <ActionIconWithConfirm
                              iconPath={mdiDeleteOutline}
                              color="alert"
                              message={t('admin.content.teams.delete', {
                                name: team.name,
                              })}
                              disabled={disabled}
                              onClick={() => onDelete(team)}
                            />
                          </Group>
                        </Table.Td>
                      </Table.Tr>
                    )
                  })}
              </Table.Tbody>
            </Table>
          </ScrollArea>
        </Box>

        <Stack hiddenFrom="sm" gap="sm" className={mobileClasses.mobileList}>
          {teams?.map((team) => {
            const members = [...(team.members ?? [])].sort((a, b) => (a.captain ? (b.captain ? 0 : -1) : 1))
            const displayName = team.name || t('common.label.team')
            const teamHeadingId = `mobile-team-${team.id}`
            const lockAction = team.locked ? t('admin.button.teams.do_unlock') : t('admin.button.teams.do_lock')

            return (
              <Card
                component="article"
                key={team.id}
                withBorder
                radius="lg"
                p="md"
                className={mobileClasses.card}
                aria-labelledby={teamHeadingId}
              >
                <Stack gap="md">
                  <Group wrap="nowrap" align="center" gap="sm">
                    <Avatar imageProps={{ loading: 'lazy' }} alt="" src={team.avatar} radius="xl" size={48}>
                      {displayName.slice(0, 1)}
                    </Avatar>
                    <Stack gap={4} className={mobileClasses.identity}>
                      <Text component="h2" id={teamHeadingId} size="sm" fw={750} className={mobileClasses.recordTitle}>
                        {displayName}
                      </Text>
                      <Badge size="sm" variant="light" color={team.locked ? 'yellow' : 'gray'} w="fit-content">
                        {team.locked ? t('admin.content.teams.locked') : t('admin.content.teams.unlocked')}
                      </Badge>
                    </Stack>
                  </Group>

                  <Box component="dl" className={mobileClasses.details}>
                    <Box component="div" className={`${mobileClasses.detail} ${mobileClasses.detailWide}`}>
                      <Text component="dt" className={mobileClasses.detailLabel}>
                        {t('admin.label.teams.bio')}
                      </Text>
                      <Text component="dd" size="sm" className={mobileClasses.detailValue}>
                        {team.bio || t('team.placeholder.bio')}
                      </Text>
                    </Box>
                    <Box component="div" className={`${mobileClasses.detail} ${mobileClasses.detailWide}`}>
                      <Text component="dt" className={mobileClasses.detailLabel}>
                        {t('admin.label.teams.members')} · {members.length}
                      </Text>
                      <Box component="dd" m={0} mt={6}>
                        <Tooltip.Group openDelay={300} closeDelay={100}>
                          <Avatar.Group spacing="sm">
                            {members.slice(0, 8).map((member) => (
                              <Tooltip key={member.id} label={member.userName} withArrow>
                                <Avatar
                                  imageProps={{ loading: 'lazy' }}
                                  alt={member.userName ?? ''}
                                  radius="xl"
                                  size="sm"
                                  src={member.avatar}
                                >
                                  {member.userName?.slice(0, 1) ?? 'U'}
                                </Avatar>
                              </Tooltip>
                            ))}
                            {members.length > 8 && (
                              <Avatar
                                aria-label={t('admin.content.teams.more_members', '{{count}} more members', {
                                  count: members.length - 8,
                                })}
                                radius="xl"
                                size="sm"
                              >
                                +{members.length - 8}
                              </Avatar>
                            )}
                          </Avatar.Group>
                        </Tooltip.Group>
                      </Box>
                    </Box>
                  </Box>

                  <Divider />
                  <Box
                    component="section"
                    aria-label={t('common.label.action', 'Actions')}
                    className={mobileClasses.actionGrid}
                    style={{ gridTemplateColumns: 'repeat(3, minmax(0, 1fr))' }}
                  >
                    <Box className={mobileClasses.actionCell}>
                      <ActionIcon
                        size={44}
                        variant="light"
                        color="blue"
                        aria-label={t('admin.button.teams.edit', 'Edit {{name}}', { name: displayName })}
                        onClick={() => {
                          setActiveTeam(team)
                          setIsEditModalOpen(true)
                        }}
                      >
                        <Icon path={mdiPencilOutline} size={1} />
                      </ActionIcon>
                      <span className={mobileClasses.actionLabel}>{t('admin.button.teams.edit')}</span>
                    </Box>
                    <Box className={mobileClasses.actionCell}>
                      <ActionIconWithConfirm
                        iconPath={team.locked ? mdiLockOpenVariantOutline : mdiLockOutline}
                        color={team.locked ? 'gray' : 'yellow'}
                        message={t('admin.content.teams.lock', {
                          name: displayName,
                          action: lockAction,
                        })}
                        disabled={disabled}
                        onClick={() => onToggleLock(team)}
                      />
                      <span className={mobileClasses.actionLabel}>{lockAction}</span>
                    </Box>
                    <Box className={mobileClasses.actionCell}>
                      <ActionIconWithConfirm
                        iconPath={mdiDeleteOutline}
                        color="alert"
                        message={t('admin.content.teams.delete', { name: displayName })}
                        disabled={disabled}
                        onClick={() => onDelete(team)}
                      />
                      <span className={mobileClasses.actionLabel}>{t('common.modal.delete')}</span>
                    </Box>
                  </Box>
                </Stack>
              </Card>
            )
          })}
        </Stack>
        <TeamEditModal
          size="min(42rem, calc(100vw - 2rem))"
          title={t('admin.button.teams.edit')}
          team={activeTeam}
          opened={isEditModalOpen}
          onClose={() => setIsEditModalOpen(false)}
          mutateTeam={(team: TeamInfoModel) => {
            updateTeams(
              [team, ...(teams?.filter((n) => n.id !== team.id) ?? [])].sort((a, b) => (a.id! < b.id! ? -1 : 1))
            )
          }}
        />
      </Paper>
    </AdminPage>
  )
}

export default Teams
