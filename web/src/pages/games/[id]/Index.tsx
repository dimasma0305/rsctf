import {
  Alert,
  Anchor,
  BackgroundImage,
  Badge,
  Button,
  Center,
  Container,
  Group,
  Stack,
  Text,
  Title,
  useMantineTheme,
} from '@mantine/core'
import { useModals } from '@mantine/modals'
import { showNotification } from '@mantine/notifications'
import {
  mdiAccountGroupOutline,
  mdiAlertCircle,
  mdiCalendarBlankOutline,
  mdiChartLine,
  mdiCheck,
  mdiFlagOutline,
  mdiLogin,
  mdiTimerSand,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { CSSProperties, FC, useEffect, useState } from 'react'
import { Trans, useTranslation } from 'react-i18next'
import { Link, useNavigate, useParams } from 'react-router'
import { GameColorMap, getGameStatusLabel } from '@Components/GameCard'
import { GameJoinModal } from '@Components/GameJoinModal'
import { GameProgress } from '@Components/GameProgress'
import { Markdown } from '@Components/MarkdownRenderer'
import { WithNavBar } from '@Components/WithNavbar'
import { useLanguage } from '@Utils/I18n'
import { showErrorMsg } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import { getGameStatus, useGame } from '@Hooks/useGame'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useTeams, useUser } from '@Hooks/useUser'
import api, { GameJoinModel, ParticipationStatus } from '@Api'
import classes from '@Styles/GameDetail.module.css'

const GetAlert = (status: ParticipationStatus, team: string) => {
  const { t } = useTranslation()

  const GameAlertMap = new Map([
    [
      ParticipationStatus.Pending,
      {
        color: 'yellow',
        icon: mdiTimerSand,
        title: t('game.participation.alert.pending.title', { team }),
        content: t('game.participation.alert.pending.content'),
      },
    ],
    [ParticipationStatus.Accepted, null],
    [
      ParticipationStatus.Rejected,
      {
        color: 'red',
        icon: mdiAlertCircle,
        title: t('game.participation.alert.rejected.title'),
        content: t('game.participation.alert.rejected.content'),
      },
    ],
    [
      ParticipationStatus.Suspended,
      {
        color: 'red',
        icon: mdiAlertCircle,
        title: t('game.participation.alert.suspended.title', { team }),
        content: t('game.participation.alert.suspended.content'),
      },
    ],
    [ParticipationStatus.Unsubmitted, null],
  ])

  const data = GameAlertMap.get(status)
  if (data) {
    return (
      <Alert color={data.color} icon={<Icon path={data.icon} />} title={data.title}>
        {data.content}
      </Alert>
    )
  }
  return null
}

const GameDetail: FC = () => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')
  const navigate = useNavigate()

  const { game, error, mutate, status } = useGame(numId)

  const theme = useMantineTheme()

  const { startTime, endTime, finished, started, progress, status: gameStatus } = getGameStatus(game)

  const { locale } = useLanguage()

  const { user } = useUser()
  const { teams } = useTeams()

  const modals = useModals()
  const isMobile = useIsMobile()

  const { t } = useTranslation()

  usePageTitle(game?.title)

  useEffect(() => {
    if (error) {
      showErrorMsg(error, t)
      navigate('/games')
    }
  }, [error, navigate])

  const [joinModalOpen, setJoinModalOpen] = useState(false)

  const GameActionMap = new Map([
    [ParticipationStatus.Pending, t('game.participation.actions.pending')],
    [ParticipationStatus.Accepted, t('game.participation.actions.accepted')],
    [ParticipationStatus.Rejected, t('game.participation.actions.rejected')],
    [ParticipationStatus.Suspended, t('game.participation.actions.suspended')],
    [ParticipationStatus.Unsubmitted, t('game.participation.actions.unsubmitted')],
  ])

  const onSubmitJoin = async (info: GameJoinModel) => {
    try {
      if (!numId) return

      await api.game.gameJoinGame(numId, info)
      showNotification({
        color: 'teal',
        message: t('game.notification.joined'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      mutate()
    } catch (err) {
      return showErrorMsg(err, t)
    }
  }

  const onSubmitLeave = async () => {
    try {
      if (!numId) return
      await api.game.gameLeaveGame(numId)

      showNotification({
        color: 'teal',
        message: t('game.notification.left'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      mutate()
    } catch (err) {
      return showErrorMsg(err, t)
    }
  }

  // Allow join if game is not finished OR practice mode is enabled
  const isGameOpenForJoin = !finished || game?.practiceMode

  const canSubmit =
    (status === ParticipationStatus.Unsubmitted || status === ParticipationStatus.Rejected) &&
    isGameOpenForJoin &&
    user &&
    teams &&
    teams.length > 0

  const teamRequire =
    user && status === ParticipationStatus.Unsubmitted && isGameOpenForJoin && teams && teams.length === 0

  const eventStatusLabel = getGameStatusLabel(t, gameStatus)
  const eventStatusColor = GameColorMap.get(gameStatus) ?? 'gray'
  const eventHue = ((game?.id ?? numId) * 47 + 186) % 360
  const eventTitle = game?.title || t('game.content.untitled', 'Untitled event')

  const onJoin = () =>
    modals.openConfirmModal({
      title: t('game.content.join.confirm'),
      children: (
        <Stack gap="xs">
          <Text size="sm">{t('game.content.join.content.0')}</Text>
          <Text size="sm">
            <Trans i18nKey="game.content.join.content.1" />
          </Text>
          <Text size="sm">
            <Trans i18nKey="game.content.join.content.2" />
          </Text>
        </Stack>
      ),
      onConfirm: () => setJoinModalOpen(true),
      confirmProps: { color: theme.primaryColor },
    })

  const onLeave = () =>
    modals.openConfirmModal({
      title: t('game.content.leave.confirm'),
      children: (
        <Stack gap="xs">
          <Text size="sm">{t('game.content.leave.content.0')}</Text>
          <Text size="sm">{t('game.content.leave.content.1')}</Text>
        </Stack>
      ),
      onConfirm: onSubmitLeave,
      confirmProps: { color: theme.primaryColor },
    })

  const ControlButtons = (
    <>
      {!user && isGameOpenForJoin ? (
        <Button
          component={Link}
          to={`/account/login?from=${encodeURIComponent(`/games/${numId}`)}`}
          leftSection={<Icon path={mdiLogin} size={0.8} aria-hidden="true" />}
        >
          {t('game.button.login_required')}
        </Button>
      ) : (
        <Button
          disabled={!canSubmit}
          onClick={onJoin}
          leftSection={<Icon path={mdiAccountGroupOutline} size={0.8} aria-hidden="true" />}
        >
          {!isGameOpenForJoin ? t('game.button.finished') : GameActionMap.get(status)}
        </Button>
      )}
      {started && (
        <Button
          component={Link}
          to={`/games/${numId}/scoreboard`}
          variant="light"
          leftSection={<Icon path={mdiChartLine} size={0.8} aria-hidden="true" />}
        >
          {t('game.button.scoreboard')}
        </Button>
      )}
      {(status === ParticipationStatus.Pending || status === ParticipationStatus.Rejected) && (
        <Button color="red" variant="outline" onClick={onLeave}>
          {t('game.button.leave')}
        </Button>
      )}
      {status === ParticipationStatus.Accepted && started && (!finished || game?.practiceMode) && (
        <Button
          component={Link}
          to={`/games/${numId}/challenges`}
          variant="light"
          leftSection={<Icon path={mdiFlagOutline} size={0.8} aria-hidden="true" />}
        >
          {t('game.button.challenges')}
        </Button>
      )}
    </>
  )

  return (
    <WithNavBar width="100%" isLoading={!game} withFooter>
      <section
        className={classes.hero}
        style={{ '--event-hue': `${eventHue}deg` } as CSSProperties}
        aria-labelledby="event-title"
      >
        <div className={classes.heroGlow} aria-hidden="true" />
        <div className={classes.heroGrid}>
          <Stack gap="lg" className={classes.heroCopy}>
            <div>
              <Text className={classes.eyebrow}>
                {t('game.content.competition_brief', 'Competition brief')} · #
                {String(game?.id ?? numId).padStart(3, '0')}
              </Text>
              <Group gap="xs" mt="xs">
                <Badge color={eventStatusColor} variant="light" size="lg" className={classes.statusBadge}>
                  <span className={classes.statusDot} data-status={gameStatus} aria-hidden="true" />
                  {eventStatusLabel}
                </Badge>
                <Badge variant="outline">
                  {!game || game.limit === 0
                    ? t('game.tag.multiplayer')
                    : game.limit === 1
                      ? t('game.tag.individual')
                      : t('game.tag.limited', { count: game.limit })}
                </Badge>
                {game?.practiceMode && (
                  <Badge variant="light" color="violet">
                    {t('game.tag.practice', 'Practice mode')}
                  </Badge>
                )}
                {game?.hidden && <Badge variant="outline">{t('game.tag.hidden')}</Badge>}
              </Group>
            </div>

            <Stack gap="xs">
              <Title id="event-title" className={classes.title}>
                {eventTitle}
              </Title>
              <Text className={classes.summary}>
                {game?.summary || t('game.content.no_summary', 'Open the event briefing for competition details.')}
              </Text>
            </Stack>

            <dl className={classes.facts}>
              <div className={classes.fact}>
                <dt>
                  <Icon path={mdiCalendarBlankOutline} size={0.86} aria-hidden="true" />
                  <span>{t('game.content.start_time')}</span>
                </dt>
                <dd>
                  <time dateTime={startTime.toISOString()}>{startTime.locale(locale).format('LLL')}</time>
                </dd>
              </div>
              <div className={classes.fact}>
                <dt>
                  <Icon path={mdiTimerSand} size={0.86} aria-hidden="true" />
                  <span>{t('game.content.end_time')}</span>
                </dt>
                <dd>
                  <time dateTime={endTime.toISOString()}>{endTime.locale(locale).format('LLL')}</time>
                </dd>
              </div>
              <div className={classes.fact}>
                <dt>
                  <Icon path={mdiAccountGroupOutline} size={0.86} aria-hidden="true" />
                  <span>{t('game.content.registered_teams', 'Registered teams')}</span>
                </dt>
                <dd>
                  <Trans i18nKey="game.content.joined_status" values={{ count: game?.teamCount ?? 0 }} />
                </dd>
              </div>
            </dl>

            <div className={classes.progressBlock}>
              <Group justify="space-between" gap="sm">
                <Text size="xs" fw={700} c="dimmed">
                  {t('game.content.event_progress_label', 'Event progress')}
                </Text>
                <Text size="xs" fw={800} className={classes.progressValue}>
                  {Math.round(Math.min(100, Math.max(0, progress)))}%
                </Text>
              </Group>
              <GameProgress
                percentage={progress}
                active={started && !finished}
                ariaLabel={t('game.content.event_progress_label', 'Event progress')}
              />
            </div>

            <Group className={classes.actions}>{ControlButtons}</Group>
          </Stack>

          <div className={classes.visual} aria-hidden="true">
            {game?.poster ? (
              <BackgroundImage className={classes.poster} src={game.poster}>
                <span className={classes.posterShade} />
              </BackgroundImage>
            ) : (
              <Center className={classes.posterFallback}>
                <span className={classes.fallbackCode}>RS::CTF / {String(game?.id ?? numId).padStart(3, '0')}</span>
                <span className={classes.fallbackIcon}>
                  <Icon path={mdiFlagOutline} size={2.1} color={theme.white} />
                </span>
                <span className={classes.fallbackLabel}>{eventStatusLabel}</span>
              </Center>
            )}
          </div>
        </div>
      </section>

      <Container fluid className={classes.content}>
        <Stack gap="md" pb={100}>
          {GetAlert(status, game?.teamName ?? '')}
          {teamRequire && (
            <Alert
              color="yellow"
              icon={<Icon path={mdiAlertCircle} />}
              title={t('game.participation.alert.team_required.title')}
            >
              <Trans i18nKey="game.participation.alert.team_required.content">
                _
                <Anchor component={Link} size="sm" to="/teams">
                  _
                </Anchor>
                _
              </Trans>
            </Alert>
          )}
          {status === ParticipationStatus.Accepted && !started && (
            <Alert color="teal" icon={<Icon path={mdiCheck} />} title={t('game.participation.alert.not_started.title')}>
              {t('game.participation.alert.not_started.content', {
                team: game?.teamName ?? '',
              })}
              {isMobile && t('game.participation.alert.not_started.mobile')}
            </Alert>
          )}
          <header className={classes.briefingHeader}>
            <Text className={classes.eyebrow}>{t('game.content.event_briefing_eyebrow', 'Mission file')}</Text>
            <Title order={2} className={classes.briefingTitle}>
              {t('game.content.event_briefing', 'Event briefing')}
            </Title>
          </header>
          {game?.content ? (
            <div className={classes.briefingBody}>
              <Markdown source={game.content} />
            </div>
          ) : (
            <Text c="dimmed">{t('game.content.no_briefing', 'No additional briefing has been published yet.')}</Text>
          )}
        </Stack>
        <GameJoinModal
          title={t('game.content.join.title')}
          opened={joinModalOpen}
          withCloseButton
          onClose={() => setJoinModalOpen(false)}
          onSubmitJoin={onSubmitJoin}
        />
      </Container>
    </WithNavBar>
  )
}

export default GameDetail
