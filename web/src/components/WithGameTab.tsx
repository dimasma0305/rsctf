import { LoadingOverlay, Stack } from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiExclamationThick } from '@mdi/js'
import { Icon } from '@mdi/react'
import React, { FC, useEffect } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, useNavigate, useParams } from 'react-router'
import { GameWorkspaceHeader } from '@Components/GameWorkspaceHeader'
import { RequireRole } from '@Components/WithRole'
import { useServerClockReady } from '@Utils/ServerClock'
import { DEFAULT_LOADING_OVERLAY } from '@Utils/Shared'
import { useGameAccess, useGameStatus } from '@Hooks/useGame'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useUserRole } from '@Hooks/useUser'
import { ParticipationStatus, Role } from '@Api'
import classes from '@Styles/GameWorkspace.module.css'

export const WithGameTab: FC<React.PropsWithChildren<{ summary?: React.ReactNode }>> = ({ children, summary }) => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')
  const location = useLocation()
  const navigate = useNavigate()

  const { role } = useUserRole()
  const { game, liveReadReady, status } = useGameAccess(numId)
  const { started, finished } = useGameStatus(game)
  const clockReady = useServerClockReady()
  const { t } = useTranslation()

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
    <Stack className={classes.competitionStack} pos="relative" mt={0} gap="sm" style={{ containerType: 'inline-size' }}>
      <LoadingOverlay visible={!game} overlayProps={DEFAULT_LOADING_OVERLAY} />
      <GameWorkspaceHeader gameId={numId} game={game} />
      {summary && <div className={classes.summary}>{summary}</div>}
      <Stack gap="sm" miw={0} data-motion="page">
        {children}
      </Stack>
    </Stack>
  )
}
