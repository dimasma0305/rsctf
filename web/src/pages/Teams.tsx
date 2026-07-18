import { Button, Center, Group, Loader, Modal, SimpleGrid, Stack, Text, TextInput, Title } from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiAccountMultiplePlus, mdiCheck, mdiClose, mdiHumanGreetingVariant } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useSearchParams } from 'react-router'
import { Empty } from '@Components/Empty'
import { PageHeader } from '@Components/PageHeader'
import { TeamCard } from '@Components/TeamCard'
import { TeamCreateModal } from '@Components/TeamCreateModal'
import { TeamEditModal } from '@Components/TeamEditModal'
import { WithNavBar } from '@Components/WithNavbar'
import { WithRole } from '@Components/WithRole'
import { showErrorMsg } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useTeams, useUser } from '@Hooks/useUser'
import api, { Role, TeamInfoModel } from '@Api'
import classes from '@Styles/Teams.module.css'

const Teams: FC = () => {
  const { user, error: userError, mutate: mutateUser } = useUser()
  const { teams, mutate: mutateTeams, error: teamsError } = useTeams()

  const [joinOpened, setJoinOpened] = useState(false)
  const [joinTeamCode, setJoinTeamCode] = useState('')
  const [joining, setJoining] = useState(false)
  const [searchParams, setSearchParams] = useSearchParams()

  // Auto-open join modal when arriving via invite link (?join=code)
  useEffect(() => {
    const code = searchParams.get('join')
    if (code) {
      setJoinTeamCode(decodeURIComponent(code))
      setJoinOpened(true)
      setSearchParams({}, { replace: true })
    }
  }, [])

  const [createOpened, setCreateOpened] = useState(false)
  const [editOpened, setEditOpened] = useState(false)

  const [editTeam, setEditTeam] = useState<TeamInfoModel | null>(null)

  const teamsOwned = teams?.filter((t) => t.members?.some((m) => m?.captain && m.id === user?.userId))
  const disallowCreate = (teamsOwned?.length ?? 0) >= 3

  const isMobile = useIsMobile()

  const { t } = useTranslation()

  usePageTitle(t('team.title.index'))

  const onEditTeam = (team: TeamInfoModel) => {
    setEditTeam(team)
    setEditOpened(true)
  }

  const codePartten = /:\d+:[0-9a-f]{32}$/

  const onJoinTeam = async () => {
    if (!codePartten.test(joinTeamCode)) {
      showNotification({
        color: 'red',
        title: t('common.error.encountered'),
        message: t('team.notification.join.wrong_invite_code'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }

    setJoining(true)
    try {
      await api.team.teamAccept(joinTeamCode)
      showNotification({
        color: 'teal',
        title: t('team.notification.join.success'),
        message: t('team.notification.updated'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      mutateTeams()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setJoining(false)
      setJoinTeamCode('')
      setJoinOpened(false)
    }
  }

  const teamActions = (className: string) => (
    <Group gap="sm" className={className}>
      <Button
        leftSection={<Icon path={mdiHumanGreetingVariant} size={1} />}
        variant="outline"
        onClick={() => setJoinOpened(true)}
      >
        {t('team.button.join')}
      </Button>
      <Button
        leftSection={<Icon path={mdiAccountMultiplePlus} size={1} />}
        variant="filled"
        onClick={() => setCreateOpened(true)}
      >
        {t('team.button.create')}
      </Button>
    </Group>
  )

  return (
    <WithNavBar>
      <WithRole requiredRole={Role.User}>
        <Stack pt="md">
          <PageHeader
            eyebrow={t('team.content.workspace', 'Your workspace')}
            title={t('team.title.index')}
            description={t(
              'team.content.index_description',
              'Create a team, join with an invite, and manage your roster.'
            )}
            actions={teamActions(classes.headerActions)}
          />
          {teamsError || userError ? (
            <Center className={classes.stateSection}>
              <Stack align="center" gap="md" className={classes.errorCard} role="alert">
                <span className={classes.errorIcon} aria-hidden="true">
                  <Icon path={mdiClose} size={1.6} />
                </span>
                <Title order={2} ta="center" style={{ wordBreak: 'break-word', hyphens: 'auto' }}>
                  {t('team.content.load_failed.title', 'Failed to load teams')}
                </Title>
                <Text size="sm" c="dimmed" ta="center" style={{ wordBreak: 'break-word', hyphens: 'auto' }}>
                  {t(
                    'team.content.load_failed.hint',
                    'Something went wrong while loading your teams. Please try again.'
                  )}
                </Text>
                <Button
                  variant="outline"
                  onClick={() => {
                    mutateTeams()
                    mutateUser()
                  }}
                >
                  {t('common.button.retry', 'Retry')}
                </Button>
              </Stack>
            </Center>
          ) : teams && user ? (
            teams.length > 0 ? (
              <SimpleGrid cols={isMobile ? 1 : 2} spacing="xl" p={isMobile ? 'sm' : '2rem'} w="100%">
                {(teams || []).map((t, i) => (
                  <TeamCard
                    key={i}
                    team={t}
                    isCaptain={t.members?.some((m) => m?.captain && m.id === user?.userId) ?? false}
                    onEdit={() => onEditTeam(t)}
                  />
                ))}
              </SimpleGrid>
            ) : (
              <Center className={classes.stateSection}>
                <div className={classes.emptyCard}>
                  <Empty
                    bordered
                    mdiPath={mdiAccountMultiplePlus}
                    title={t('team.content.no_team.title')}
                    description={t('team.content.no_team.hint')}
                    action={teamActions(classes.emptyActions)}
                  />
                </div>
              </Center>
            )
          ) : (
            <Center className={classes.stateSection}>
              <Stack align="center" gap="sm" role="status" aria-live="polite">
                <Loader aria-hidden="true" />
                <Text size="sm" c="dimmed">
                  {t('team.content.loading', 'Loading teams…')}
                </Text>
              </Stack>
            </Center>
          )}
        </Stack>

        <Modal opened={joinOpened} title={t('team.button.join')} onClose={() => setJoinOpened(false)}>
          <Stack>
            <Text size="sm">{t('team.content.join')}</Text>
            <TextInput
              label={t('team.label.invite_code')}
              type="text"
              placeholder="team:0:01234567890123456789012345678901"
              w="100%"
              value={joinTeamCode}
              onChange={(event) => setJoinTeamCode(event.currentTarget.value)}
            />
            <Button fullWidth variant="outline" loading={joining} disabled={joining} onClick={onJoinTeam}>
              {t('team.button.join')}
            </Button>
          </Stack>
        </Modal>

        <TeamCreateModal
          opened={createOpened}
          title={t('team.button.create')}
          disallowCreate={disallowCreate ?? false}
          onClose={() => setCreateOpened(false)}
          mutate={mutateTeams}
        />

        <TeamEditModal
          opened={editOpened}
          title={t('team.button.edit')}
          onClose={() => setEditOpened(false)}
          team={editTeam}
          isCaptain={editTeam?.members?.some((m) => m?.captain && m.id === user?.userId) ?? false}
        />
      </WithRole>
    </WithNavBar>
  )
}

export default Teams
