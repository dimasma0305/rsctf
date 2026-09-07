import { Badge, Card, Group, LoadingOverlay, Stack, Text, Title } from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiChartLine, mdiExclamationThick, mdiFlagOutline, mdiMonitorEye, mdiUpload } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import duration from 'dayjs/plugin/duration'
import React, { FC, useEffect } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, useNavigate, useParams } from 'react-router'
import { GameProgress } from '@Components/GameProgress'
import { IconTabs } from '@Components/IconTabs'
import { RequireRole } from '@Components/WithRole'
import { useServerClockReady } from '@Utils/ServerClock'
import { DEFAULT_LOADING_OVERLAY } from '@Utils/Shared'
import { isReadOnlyGameArchive } from '@Utils/gameArchive'
import { useGameAccess, useGameStatus } from '@Hooks/useGame'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useUserRole } from '@Hooks/useUser'
import { DetailedGameInfoModel, ParticipationStatus, Role } from '@Api'
import classes from '@Styles/GameWorkspace.module.css'
import misc from '@Styles/Misc.module.css'

dayjs.extend(duration)

const GameCountdown: FC<{ game?: DetailedGameInfoModel; compact?: boolean }> = ({ game, compact }) => {
  const { endTime, progress, started, finished, now } = useGameStatus(game)

  const { t } = useTranslation()

  const countdown = dayjs.duration(endTime.diff(now))

  // An ended event already has a status badge. Do not leave a "time remaining"
  // instrument showing a redundant end message in the competition header.
  if (compact && finished) return null

  return (
    <Card
      miw="9rem"
      ta="center"
      p={compact ? 0 : undefined}
      pt={compact ? 0 : 4}
      role="timer"
      aria-live="off"
      aria-label={t('game.content.time_remaining', 'Game time remaining')}
      className={compact ? classes.countdown : misc.overflowVisible}
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

export const WithGameTab: FC<React.PropsWithChildren<{ summary?: React.ReactNode }>> = ({ children, summary }) => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')
  const location = useLocation()
  const navigate = useNavigate()

  const { role } = useUserRole()
  const { game, liveReadReady, status } = useGameAccess(numId)
  const { started, finished, now } = useGameStatus(game)
  const clockReady = useServerClockReady()
  const { t } = useTranslation()

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
      requireJoin: false,
      requireRole: Role.User,
    },
    {
      icon: mdiUpload,
      title: t('game.tab.submit', 'Submit'),
      path: 'submit',
      link: 'submit',
      requireJoin: false,
      requireRole: Role.User,
      hidden: game?.allowUserSubmissions === false || archived,
    },
    {
      icon: mdiMonitorEye,
      title: t('game.tab.monitor.index'),
      path: 'monitor',
      link: 'monitor/events',
      requireJoin: false,
      requireRole: Role.Monitor,
    },
  ]

  const filteredPages = pages
    .filter((p) => !p.hidden)
    .filter((p) => RequireRole(p.requireRole, role))
    .filter((p) => !p.requireJoin || game?.status === ParticipationStatus.Accepted)

  const tabs = filteredPages.map((p) => ({
    tabKey: p.link,
    to: `/games/${numId}/${p.link}`,
    label: p.title,
    icon: <Icon path={p.icon} size={1} />,
  }))
  const getTab = (path: string) => filteredPages?.findIndex((page) => path.includes(page.path))

  const activeTab = Math.max(0, getTab(location.pathname))

  usePageTitle(game?.title)

  useEffect(() => {
    if (game && clockReady && liveReadReady) {
      if (location.pathname.includes('monitor') && role === undefined) return

      if (!started) {
        navigate(`/games/${numId}`)
        showNotification({
          id: 'no-access',
          color: 'yellow',
          message: t('game.notification.not_started'),
          icon: <Icon path={mdiExclamationThick} size={1} />,
        })
        return
      }

      if (location.pathname.includes('scoreboard')) {
        // allow access to scoreboard
        return
      }

      if (location.pathname.includes('challenges') && status === ParticipationStatus.Accepted) {
        // Accepted participants retain a read-only archive after closeout.
        return
      }

      if (location.pathname.includes('monitor') && RequireRole(Role.Monitor, role)) {
        // allow access to monitor
        return
      }

      // Protected routes handle anonymous visitors through the global session
      // redirect. Do not show the participation-specific "not joined" warning
      // before the visitor has even signed in.
      if (role === undefined) return

      if (!finished) {
        if (status === ParticipationStatus.Suspended) {
          navigate(`/games/${numId}`)
          showNotification({
            id: 'no-access',
            color: 'yellow',
            message: t('game.notification.suspended'),
            icon: <Icon path={mdiExclamationThick} size={1} />,
          })
        } else if (status !== ParticipationStatus.Accepted) {
          navigate(`/games/${numId}`)
          showNotification({
            id: 'no-access',
            color: 'yellow',
            message: t('game.notification.not_joined'),
            icon: <Icon path={mdiExclamationThick} size={1} />,
          })
        }
      } else if (!game.practiceMode && !RequireRole(Role.Monitor, role)) {
        // not allow access to game after it ends if:
        // 1. not monitor
        // 2. not practice mode
        navigate(`/games/${numId}`)
        showNotification({
          id: 'no-access',
          color: 'yellow',
          message: t('game.notification.ended'),
          icon: <Icon path={mdiExclamationThick} size={1} />,
        })
      }
    }
  }, [clockReady, finished, game, liveReadReady, location, navigate, numId, role, started, status, t])

  return (
    <Stack
      className={summary ? classes.competitionStack : undefined}
      pos="relative"
      mt={summary ? 0 : 'md'}
      gap={summary ? 'sm' : 'md'}
      style={{ containerType: 'inline-size' }}
    >
      <LoadingOverlay visible={!game} overlayProps={DEFAULT_LOADING_OVERLAY} />
      <div
        className={summary ? classes.masthead : classes.standardHeading}
        data-event-workspace-header={summary ? true : undefined}
      >
        {game && (
          <header className={classes.header} data-competition={summary ? true : undefined}>
            <Stack gap={4} miw={0}>
              <Group gap="xs" className={classes.eventIdentity}>
                <Text size="xs" c="dimmed" className={classes.eventId}>
                  {t('common.workspace.event_id', 'Event #{{id}}', { id: numId })}
                </Text>
                <Badge variant="light" color={finished ? 'gray' : started ? 'green' : 'blue'}>
                  {finished
                    ? t('game.arena.ended', 'Ended')
                    : started
                      ? t('game.arena.live', 'Live')
                      : t('game.arena.upcoming', 'Upcoming')}
                </Badge>
              </Group>
              <Title className={classes.title}>{game.title}</Title>
            </Stack>
            <GameCountdown game={game} compact={!!summary} />
          </header>
        )}
        {summary && <div className={classes.summary}>{summary}</div>}
        <div className={summary ? classes.eventNavigation : undefined}>
          <IconTabs
            mode="navigation"
            appearance={summary ? 'underline' : 'surface'}
            position="flex-start"
            ariaLabel={t('game.tab.navigation', 'Game sections')}
            active={activeTab}
            tabs={tabs}
          />
        </div>
      </div>
      {children}
    </Stack>
  )
}
