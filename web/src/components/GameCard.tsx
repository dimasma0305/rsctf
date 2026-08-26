import { Badge, Card, Group, Image, Stack, Text, Title } from '@mantine/core'
import {
  mdiAccountCheckOutline,
  mdiAccountClockOutline,
  mdiAccountGroupOutline,
  mdiAccountOffOutline,
  mdiAccountOutline,
  mdiCalendarBlankOutline,
  mdiClockOutline,
  mdiFlagOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { TFunction } from 'i18next'
import { CSSProperties, FC } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { useLanguage } from '@Utils/I18n'
import { toLimitTag, useGameStatus } from '@Hooks/useGame'
import { BasicGameInfoModel, ParticipationStatus } from '@Api'
import classes from '@Styles/GameCard.module.css'

export enum GameStatus {
  Coming = 'coming',
  OnGoing = 'ongoing',
  Ended = 'ended',
}

export const GameColorMap = new Map<GameStatus, string>([
  [GameStatus.Coming, 'yellow'],
  [GameStatus.OnGoing, 'green'],
  [GameStatus.Ended, 'blue'],
])

export const getGameStatusLabel = (t: TFunction, status: GameStatus) => {
  switch (status) {
    case GameStatus.OnGoing:
      return t('game.content.lifecycle.live', 'Live now')
    case GameStatus.Coming:
      return t('game.content.lifecycle.upcoming', 'Upcoming')
    case GameStatus.Ended:
      return t('game.content.lifecycle.past', 'Completed')
  }
}

interface GameCardProps {
  game: BasicGameInfoModel
  showMembership?: boolean
}

export const GameCard: FC<GameCardProps> = ({ game, showMembership = false, ...others }) => {
  const { t } = useTranslation()
  const { locale } = useLanguage()

  const { summary, title, poster, limit, teamCount, userCount } = game
  const { startTime, endTime, status } = useGameStatus(game)
  const durationMinutes = Math.max(0, endTime.diff(startTime, 'minute'))
  const durationLabel =
    durationMinutes >= 48 * 60
      ? t('game.content.duration_days', {
          defaultValue: '{{count}} days',
          count: Math.ceil(durationMinutes / (24 * 60)),
        })
      : durationMinutes < 60
        ? t('game.content.duration_minutes', {
            defaultValue: '{{count}} min',
            count: Math.max(1, durationMinutes),
          })
        : t('game.content.duration', { hours: Math.ceil(durationMinutes / 60) })
  const color = GameColorMap.get(status) ?? 'gray'
  const statusLabel = getGameStatusLabel(t, status)
  const eventTitle = title || t('game.content.untitled', 'Untitled event')
  const eventHue = (game.id * 47 + 186) % 360
  const membership = (() => {
    switch (game.participationStatus) {
      case ParticipationStatus.Accepted:
        return { status: 'accepted', icon: mdiAccountCheckOutline, label: t('game.content.joined', 'Joined') }
      case ParticipationStatus.Pending:
        return { status: 'pending', icon: mdiAccountClockOutline, label: t('game.content.join_pending', 'Pending') }
      case ParticipationStatus.Suspended:
        return { status: 'suspended', icon: mdiAccountOffOutline, label: t('game.content.join_suspended', 'Suspended') }
      case ParticipationStatus.Rejected:
        return { status: 'rejected', icon: mdiAccountOffOutline, label: t('game.content.join_rejected', 'Rejected') }
      default:
        return { status: 'none', icon: mdiAccountOutline, label: t('game.content.not_joined', 'Not joined') }
    }
  })()

  return (
    <Card {...others} component="article" className={classes.root}>
      <Link to={`/games/${game.id}`} className={classes.link} data-guide="event-card">
        <div className={classes.visual} style={{ '--event-hue': `${eventHue}deg` } as CSSProperties}>
          {poster ? (
            <Image src={poster} alt="" />
          ) : (
            <div className={classes.posterFallback} aria-hidden="true">
              <span className={classes.posterCode}>#{String(game.id).padStart(3, '0')}</span>
              <span className={classes.posterSignal}>
                <Icon path={mdiFlagOutline} size={1.35} />
              </span>
              <span className={classes.posterMark}>RS::CTF</span>
            </div>
          )}
          <span className={classes.status} data-status={status}>
            <span className={classes.statusDot} aria-hidden="true" />
            {statusLabel}
          </span>
          {showMembership && (
            <span className={classes.membership} data-membership={membership.status}>
              <Icon path={membership.icon} size={0.62} aria-hidden="true" />
              {membership.label}
            </span>
          )}
        </div>

        <div className={classes.content}>
          <Stack gap={7} className={classes.copy}>
            <Title order={4} size="h4" lineClamp={2} className={classes.title} title={eventTitle}>
              {eventTitle}
            </Title>
            <Text
              size="sm"
              lineClamp={2}
              className={classes.summary}
              title={summary || t('game.content.no_summary', 'Open the event to view competition details.')}
            >
              {summary || t('game.content.no_summary', 'Open the event to view competition details.')}
            </Text>
          </Stack>

          <div className={classes.schedule}>
            <Icon path={mdiCalendarBlankOutline} size={0.78} aria-hidden="true" />
            <div className={classes.scheduleDates}>
              <Text
                component="time"
                dateTime={startTime.toISOString()}
                size="xs"
                fw={650}
                title={startTime.locale(locale).format('L LTS')}
              >
                {startTime.locale(locale).format('L LTS')}
              </Text>
              <Text
                component="time"
                dateTime={endTime.toISOString()}
                size="xs"
                c="dimmed"
                title={endTime.locale(locale).format('L LTS')}
              >
                {t('game.content.until', 'until {{time}}', { time: endTime.locale(locale).format('L LTS') })}
              </Text>
            </div>
          </div>

          <Group gap={6} className={classes.metadata}>
            <Badge size="sm" color={color} variant="light">
              {toLimitTag(t, limit)}
            </Badge>
            <Badge
              size="sm"
              color="gray"
              variant="light"
              leftSection={<Icon path={mdiClockOutline} size={0.58} aria-hidden="true" />}
            >
              {durationLabel}
            </Badge>
            {(teamCount !== undefined || userCount !== undefined) && (
              <Badge
                size="sm"
                color="gray"
                variant="light"
                leftSection={<Icon path={mdiAccountGroupOutline} size={0.58} aria-hidden="true" />}
              >
                {t('game.content.event_participants', '{{teams}} teams · {{users}} players', {
                  teams: teamCount ?? 0,
                  users: userCount ?? 0,
                })}
              </Badge>
            )}
          </Group>
        </div>
      </Link>
    </Card>
  )
}
