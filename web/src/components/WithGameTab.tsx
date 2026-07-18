import { Card, LoadingOverlay, Stack, Text, Title } from '@mantine/core'
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
import { DEFAULT_LOADING_OVERLAY } from '@Utils/Shared'
import { getGameStatus, useGame } from '@Hooks/useGame'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useTicker } from '@Hooks/useTicker'
import { useUserRole } from '@Hooks/useUser'
import { DetailedGameInfoModel, ParticipationStatus, Role } from '@Api'
import misc from '@Styles/Misc.module.css'

dayjs.extend(duration)

const GameCountdown: FC<{ game?: DetailedGameInfoModel }> = ({ game }) => {
  const { endTime, progress, started, finished } = getGameStatus(game)
  // Shared 1s ticker: single global interval for every countdown on the page.
  const now = useTicker()

  const { t } = useTranslation()

  const countdown = dayjs.duration(endTime.diff(now))

  return (
    <Card
      miw="9rem"
      ta="center"
      pt={4}
      role="timer"
      aria-live="off"
      aria-label={t('game.content.time_remaining', 'Game time remaining')}
      className={misc.overflowVisible}
    >
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

export const WithGameTab: FC<React.PropsWithChildren> = ({ children }) => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')
  const location = useLocation()
  const navigate = useNavigate()

  const { role } = useUserRole()
  const { game, status } = useGame(numId)
  const { t } = useTranslation()

  const finished = dayjs() > dayjs(game?.end ?? new Date())

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
      hidden: game?.allowUserSubmissions === false,
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
    .filter((p) => !p.requireJoin || !finished || game?.practiceMode)

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
    if (game) {
      if (location.pathname.includes('monitor') && role === undefined) return

      const now = dayjs()
      if (now < dayjs(game.start)) {
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

      if (location.pathname.includes('monitor') && RequireRole(Role.Monitor, role)) {
        // allow access to monitor
        return
      }

      // Protected routes handle anonymous visitors through the global session
      // redirect. Do not show the participation-specific "not joined" warning
      // before the visitor has even signed in.
      if (role === undefined) return

      if (now < dayjs(game.end)) {
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
  }, [game, status, role, location])

  return (
    <Stack pos="relative" mt="md" style={{ containerType: 'inline-size' }}>
      <LoadingOverlay visible={!game} overlayProps={DEFAULT_LOADING_OVERLAY} />
      <IconTabs
        mode="navigation"
        ariaLabel={t('game.tab.navigation', 'Game sections')}
        active={activeTab}
        tabs={tabs}
        aside={
          game && (
            <>
              <Title>{game?.title}</Title>
              <GameCountdown game={game} />
            </>
          )
        }
      />
      {children}
    </Stack>
  )
}
