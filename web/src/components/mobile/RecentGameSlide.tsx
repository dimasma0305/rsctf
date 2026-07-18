import { Badge, Group, Paper, Stack, Text, Title, useMantineTheme } from '@mantine/core'
import { mdiFlagOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { GameColorMap, GameStatus, getGameStatusLabel } from '@Components/GameCard'
import { RecentGameProps } from '@Components/RecentGame'
import { useLanguage } from '@Utils/I18n'
import { getGameStatus, toLimitTag } from '@Hooks/useGame'
import classes from '@Styles/RecentGameSlide.module.css'

export const RecentGameSlide: FC<RecentGameProps> = ({ game, ...others }) => {
  const { title, poster, summary, limit } = game
  const { startTime, endTime, status } = getGameStatus(game)
  const { t } = useTranslation()
  const { locale } = useLanguage()
  const theme = useMantineTheme()
  const color = GameColorMap.get(status)
  const durationMinutes = Math.max(
    0,
    status === GameStatus.OnGoing ? endTime.diff(Date.now(), 'minute') : endTime.diff(startTime, 'minute')
  )
  const compactDuration =
    durationMinutes < 60
      ? `${Math.max(1, durationMinutes)}m`
      : durationMinutes < 24 * 60
        ? `${Math.ceil(durationMinutes / 60)}h`
        : `${Math.ceil(durationMinutes / (24 * 60))}d`
  const statusText = getGameStatusLabel(t, status)

  return (
    <Paper
      {...others}
      component={Link}
      to={`/games/${game.id}`}
      shadow="md"
      p="md"
      data-image={poster || undefined}
      style={
        poster
          ? {
              backgroundImage: `linear-gradient(180deg, rgba(4, 8, 15, 0.08), rgba(4, 8, 15, 0.92)), url(${poster})`,
            }
          : undefined
      }
      className={classes.card}
    >
      <Stack h="100%" gap="sm" justify="space-between" className={classes.content}>
        <Group gap={5} justify="space-between" align="flex-start">
          <Group gap={5}>
            <Badge size="sm" variant="filled" color={color}>
              {statusText}
            </Badge>
            <Badge size="sm" variant="light" color="gray">
              {toLimitTag(t, limit)}
            </Badge>
          </Group>
          <Badge size="sm" variant="light" color={color}>
            {status === GameStatus.OnGoing
              ? t('game.content.remaining_compact', '{{duration}} left', {
                  duration: compactDuration,
                })
              : t('game.content.duration_compact', '{{duration}} total', {
                  duration: compactDuration,
                })}
          </Badge>
        </Group>

        {!poster && (
          <span className={classes.signal} aria-hidden="true">
            <Icon path={mdiFlagOutline} size={2.7} color={theme.colors.gray[5]} />
          </span>
        )}

        <Stack gap={4} className={classes.copy}>
          <Text size="xs" fw={700} tt="uppercase" className={classes.date}>
            {status === GameStatus.Coming
              ? t('game.content.starts_compact', 'Starts {{time}}', {
                  time: startTime.locale(locale).format('MMM D · LT'),
                })
              : t('game.content.ends_compact', 'Ends {{time}}', {
                  time: endTime.locale(locale).format('MMM D · LT'),
                })}
          </Text>
          <Title order={3} className={classes.title}>
            {title}
          </Title>
          {summary && (
            <Text size="sm" lineClamp={2} className={classes.summary}>
              {summary}
            </Text>
          )}
        </Stack>
      </Stack>
    </Paper>
  )
}
