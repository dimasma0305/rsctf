import { Button, Center, Group, Loader, SimpleGrid, Stack, Text, Title } from '@mantine/core'
import { mdiAccountMultiplePlus, mdiClose, mdiHumanGreetingVariant } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useSearchParams } from 'react-router'
import { Empty } from '@Components/Empty'
import { PageHeader } from '@Components/PageHeader'
import { TeamCard } from '@Components/TeamCard'
import { TeamCreateModal } from '@Components/TeamCreateModal'
import { TeamEditModal } from '@Components/TeamEditModal'
import { TeamJoinModal } from '@Components/TeamJoinModal'
import { WithNavBar } from '@Components/WithNavbar'
import { WithRole } from '@Components/WithRole'
import { usePlayerGuide } from '@Components/guide/PlayerGuide'
import { useIsMobile } from '@Utils/ThemeOverride'
import { useConfig } from '@Hooks/useConfig'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useTeams, useUser } from '@Hooks/useUser'
import { Role, TeamInfoModel } from '@Api'
import classes from '@Styles/Teams.module.css'

const Teams: FC = () => {
  const { user, error: userError, mutate: mutateUser } = useUser()
  const { teams, mutate: mutateTeams, error: teamsError } = useTeams()
  const { config } = useConfig()
  const { preferences: guidePreferences, completeTeamSetup } = usePlayerGuide()

  const [joinOpened, setJoinOpened] = useState(false)
  const [joinTeamCode, setJoinTeamCode] = useState('')
  const [searchParams, setSearchParams] = useSearchParams()
  const activeAccountId = useRef<string | null>(null)
  const nextAccountId = user?.userId ?? null
  const accountChanged = Boolean(activeAccountId.current && activeAccountId.current !== nextAccountId)

  useEffect(() => {
    const previousAccountId = activeAccountId.current

    if (previousAccountId && previousAccountId !== nextAccountId) {
      setJoinTeamCode('')
      setJoinOpened(false)
    }
    activeAccountId.current = nextAccountId
  }, [nextAccountId])

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

  useEffect(() => {
    if ((teams?.length ?? 0) > 0 && guidePreferences.activeTourStep === 'team') completeTeamSetup()
  }, [completeTeamSetup, guidePreferences.activeTourStep, teams?.length])

  const teamsOwned = teams?.filter((t) => t.members?.some((m) => m?.captain && m.id === user?.userId))
  const disallowCreate = (teamsOwned?.length ?? 0) >= 3

  const isMobile = useIsMobile()

  const { t } = useTranslation()

  usePageTitle(t('team.title.index'))

  const onEditTeam = (team: TeamInfoModel) => {
    setEditTeam(team)
    setEditOpened(true)
  }

  const teamActions = (className: string) => (
    <Group gap="sm" className={className}>
      <Button
        leftSection={<Icon path={mdiHumanGreetingVariant} size={1} />}
        variant="outline"
        onClick={() => setJoinOpened(true)}
        data-guide="team-join"
      >
        {t('team.button.join')}
      </Button>
      <Button
        leftSection={<Icon path={mdiAccountMultiplePlus} size={1} />}
        variant="filled"
        onClick={() => setCreateOpened(true)}
        data-guide="team-create"
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

        <TeamJoinModal
          key={user?.userId ?? 'account-pending'}
          opened={joinOpened && !accountChanged}
          title={t('team.button.join')}
          code={accountChanged ? '' : joinTeamCode}
          onCodeChange={setJoinTeamCode}
          onClose={() => {
            setJoinTeamCode('')
            setJoinOpened(false)
          }}
          mutate={mutateTeams}
          onTeamReady={completeTeamSetup}
          enableBrowserFingerprint={config.enableBrowserFingerprint}
          apiPublicKey={config.apiPublicKey}
        />

        <TeamCreateModal
          opened={createOpened}
          title={t('team.button.create')}
          disallowCreate={disallowCreate ?? false}
          onClose={() => setCreateOpened(false)}
          mutate={mutateTeams}
          onTeamReady={completeTeamSetup}
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
