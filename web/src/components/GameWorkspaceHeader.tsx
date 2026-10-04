import { Badge, Card, Group, Stack, Text, Title } from '@mantine/core'
import { mdiChartLine, mdiEarth, mdiFlagOutline, mdiMonitorEye, mdiUpload } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import duration from 'dayjs/plugin/duration'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation } from 'react-router'
import { GameProgress } from '@Components/GameProgress'
import { IconTabs } from '@Components/IconTabs'
import { RequireRole } from '@Components/WithRole'
import { isReadOnlyGameArchive } from '@Utils/gameArchive'
import { useGameStatus } from '@Hooks/useGame'
import { useUserRole } from '@Hooks/useUser'
import { DetailedGameInfoModel, ParticipationStatus, Role } from '@Api'
import classes from '@Styles/GameWorkspace.module.css'

dayjs.extend(duration)

const GameCountdown: FC<{ game: DetailedGameInfoModel }> = ({ game }) => {
  const { endTime, progress, started, finished, now } = useGameStatus(game)
  const { t } = useTranslation()
  const countdown = dayjs.duration(endTime.diff(now))

  if (finished || (started && game.practiceMode)) return null

  return (
    <Card
      miw="9rem"
      ta="center"
      p={0}
      role="timer"
      aria-live="off"
      aria-label={t('game.content.time_remaining', 'Game time remaining')}
      className={classes.countdown}
    >
      <Text size="xs" c="dimmed">
        {t('game.content.time_remaining', 'Time remaining')}
      </Text>
      <Text fw="bold" lineClamp={1}>
        {countdown.asHours() > 999
          ? t('game.content.game_lasts_long')
          : countdown.asSeconds() > 0
            ? `${Math.floor(countdown.asHours())} : ${countdown.format('mm : ss')}`
            : t('game.content.game_ended')}
      </Text>
      <Card.Section mt={4}>
        <GameProgress
          percentage={progress}
          active={started && !finished}
          ariaLabel={t('game.content.event_progress_label', 'Event progress')}
          py={0}
        />
      </Card.Section>
    </Card>
  )
}

/** Presentation only: each route retains ownership of its reads and access checks. */
export const GameWorkspaceHeader: FC<{ gameId: number; game?: DetailedGameInfoModel }> = ({ gameId, game }) => {
  const { t } = useTranslation()
  const { pathname } = useLocation()
  const { role } = useUserRole()
  const { started, finished, now } = useGameStatus(game)
  const archived = isReadOnlyGameArchive(game, now.valueOf())
  const pages = [
    {
      icon: mdiFlagOutline,
      title: t('game.tab.challenge'),
      path: 'challenges',
      link: 'challenges',
      requireJoin: true,
      requireRole: Role.User,
    },
    {
      icon: mdiChartLine,
      title: t('game.tab.scoreboard'),
      path: 'scoreboard',
      link: 'scoreboard',
      requireRole: Role.User,
    },
    {
      icon: mdiEarth,
      title: t('game.tab.live_arena', 'Live arena'),
      path: 'attack',
      link: 'attack',
      requireRole: Role.User,
    },
    {
      icon: mdiUpload,
      title: t('game.tab.submit', 'Submit'),
      path: 'submit',
      link: 'submit',
      requireRole: Role.User,
      hidden: game?.allowUserSubmissions === false || archived,
    },
    {
      icon: mdiMonitorEye,
      title: t('game.tab.monitor.index'),
      path: 'monitor',
      link: 'monitor/events',
      requireRole: Role.Monitor,
    },
  ].filter(
    (page) =>
      !page.hidden &&
      RequireRole(page.requireRole, role) &&
      (!page.requireJoin || game?.status === ParticipationStatus.Accepted)
  )
  const active = Math.max(
    0,
    pages.findIndex((page) => pathname.split('/')[3] === page.path)
  )

  return (
    <div className={classes.masthead} data-event-workspace-header>
      <header className={classes.header}>
        <Stack gap={4} miw={0}>
          <Group gap="xs" className={classes.eventIdentity}>
            <Text size="xs" c="dimmed" className={classes.eventId}>
              {t('common.workspace.event_id', 'Event #{{id}}', { id: gameId })}
            </Text>
            {game && (
              <Badge
                variant="light"
                color={started && game.practiceMode ? 'cyan' : finished ? 'gray' : started ? 'green' : 'blue'}
              >
                {started && game.practiceMode
                  ? t('game.arena.practice', 'Practice')
                  : finished
                    ? t('game.arena.ended', 'Ended')
                    : started
                      ? t('game.arena.live', 'Live')
                      : t('game.arena.upcoming', 'Upcoming')}
              </Badge>
            )}
          </Group>
          <Title className={classes.title}>
            {game?.title ?? t('common.workspace.event_id', 'Event #{{id}}', { id: gameId })}
          </Title>
        </Stack>
        {game && <GameCountdown game={game} />}
      </header>
      <div className={classes.eventNavigation} data-event-tabs>
        <IconTabs
          mode="navigation"
          appearance="underline"
          position="flex-start"
          ariaLabel={t('game.tab.navigation', 'Game sections')}
          active={active}
          tabs={pages.map((page) => ({
            tabKey: page.link,
            to: `/games/${gameId}/${page.link}`,
            label: page.title,
            icon: <Icon path={page.icon} size={1} />,
          }))}
        />
      </div>
    </div>
  )
}
