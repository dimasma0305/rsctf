import { Badge, Box, Card, Group, Progress, Stack, Text, Title, Tooltip, useMantineTheme } from '@mantine/core'
import { mdiFlagOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { GameColorMap, GameStatus, getGameStatusLabel } from '@Components/GameCard'
import { useLanguage } from '@Utils/I18n'
import { getGameStatus, toLimitTag } from '@Hooks/useGame'
import { BasicGameInfoModel } from '@Api'
import classes from '@Styles/RecentGame.module.css'

export interface RecentGameProps {
  game: BasicGameInfoModel
}

export const RecentGame: FC<RecentGameProps> = ({ game, ...others }) => {
  const { t } = useTranslation()
  const { locale } = useLanguage()
  const theme = useMantineTheme()
  const { title, poster, limit } = game
  const { startTime, endTime, status, progress } = getGameStatus(game)
  const color = GameColorMap.get(status)
  const referenceTime = status === GameStatus.Coming ? startTime : endTime
  const statusText = getGameStatusLabel(t, status)

  return (
    <Card
      {...others}
      component={Link}
      to={`/games/${game.id}`}
      withBorder
      padding="sm"
      className={classes.card}
      aria-label={`${title ?? t('game.content.recent_games.untitled', 'Untitled game')} — ${statusText}`}
    >
      <Group wrap="nowrap" align="stretch" gap="sm">
        <Box
          className={classes.visual}
          data-image={poster || undefined}
          style={poster ? { backgroundImage: `url(${poster})` } : undefined}
          aria-hidden="true"
        >
          {!poster && <Icon path={mdiFlagOutline} size={1.8} color={theme.colors.gray[5]} />}
        </Box>

        <Stack gap={5} className={classes.copy}>
          <Group gap={5} wrap="nowrap" justify="space-between">
            <Group gap={5} wrap="nowrap">
              <Badge size="xs" color={color} variant="light">
                {statusText}
              </Badge>
              <Badge size="xs" color="gray" variant="light">
                {toLimitTag(t, limit)}
              </Badge>
            </Group>
          </Group>

          <Tooltip label={title} withArrow disabled={!title}>
            <Title order={3} size="md" lineClamp={1} className={classes.title}>
              {title ?? t('game.content.recent_games.untitled', 'Untitled game')}
            </Title>
          </Tooltip>

          <Text size="xs" c="dimmed" className={classes.meta}>
            {status === GameStatus.Coming
              ? t('game.content.starts_compact', 'Starts {{time}}', {
                  time: referenceTime.locale(locale).format('MMM D · LT'),
                })
              : t('game.content.ends_compact', 'Ends {{time}}', {
                  time: referenceTime.locale(locale).format('MMM D · LT'),
                })}
          </Text>

          {status === GameStatus.OnGoing && (
            <Progress
              value={progress}
              color={color}
              size={4}
              radius="xl"
              aria-label={t('game.content.event_progress', 'Event progress: {{value}}%', {
                value: Math.round(progress),
              })}
            />
          )}
        </Stack>
      </Group>
    </Card>
  )
}
