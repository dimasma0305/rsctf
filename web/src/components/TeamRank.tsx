import {
  Avatar,
  Badge,
  Button,
  Card,
  CardProps,
  Group,
  PasswordInput,
  Popover,
  Progress,
  Skeleton,
  Stack,
  Text,
  Title,
  UnstyledButton,
} from '@mantine/core'
import { useClipboard } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiChevronDown, mdiExclamationThick, mdiKey, mdiOpenInNew, mdiSword } from '@mdi/js'
import { Icon } from '@mdi/react'
import cx from 'clsx'
import { FC, useEffect, useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate, useParams } from 'react-router'
import { isLiveChallenge } from '@Components/competition/model'
import { ErrorCodes } from '@Utils/Shared'
import { visibleChallengeSolveProgress } from '@Utils/challengeProgress'
import { isReadOnlyGameArchive } from '@Utils/gameArchive'
import { useGameStatus, useGameTeamInfo } from '@Hooks/useGame'
import classes from '@Styles/GameWorkspace.module.css'
import misc from '@Styles/Misc.module.css'

type TeamRankProps = CardProps & {
  teamState: ReturnType<typeof useGameTeamInfo>
  compact?: boolean
}

export const TeamRank: FC<TeamRankProps> = ({ teamState, compact = false, ...props }) => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')
  const navigate = useNavigate()
  const { teamInfo, game, error } = teamState
  const { now: serverNow } = useGameStatus(game)
  const archived = isReadOnlyGameArchive(game, serverNow.valueOf())

  const clipboard = useClipboard()
  const { t } = useTranslation()

  const solvedProgress = visibleChallengeSolveProgress(teamInfo?.rank?.solvedCount, teamInfo?.challengeCount)

  const division = useMemo(() => {
    if (teamInfo?.rank?.divisionId && game?.divisions) {
      const division = game.divisions.find((d) => d.id === teamInfo.rank!.divisionId)
      return division?.name ?? ''
    }

    return null
  }, [teamInfo?.rank?.divisionId, game?.divisions])

  useEffect(() => {
    if (error?.status === ErrorCodes.GameEnded) {
      navigate(`/games/${numId}`)
      showNotification({
        color: 'yellow',
        message: t('game.notification.ended'),
        icon: <Icon path={mdiExclamationThick} size={1} />,
      })
    }
  }, [error])

  const rank = teamInfo?.rank

  const copyTeamToken = () => {
    clipboard.copy(teamInfo?.teamToken)
    showNotification({
      color: 'teal',
      message: t('team.notification.token.copied'),
      icon: <Icon path={mdiCheck} size={1} />,
    })
  }

  const item = (label: string, value?: null | string | number) => (
    <Stack gap={2}>
      <Skeleton visible={!rank}>
        <Text ff="monospace" fw="bold">
          {value ?? '0'}
        </Text>
      </Skeleton>
      <Text size="xs" fw={500}>
        {label}
      </Text>
    </Stack>
  )

  if (compact) {
    const challenges = Object.values(teamInfo?.challenges ?? {}).flat()
    const hasLive = challenges.some(isLiveChallenge)
    const hasJeopardy = challenges.some((challenge) => !isLiveChallenge(challenge))
    return (
      <div className={classes.teamStrip} data-team-summary data-mixed-score={(hasLive && hasJeopardy) || undefined}>
        <Popover position="bottom-start" width="min(22rem, calc(100vw - 2rem))" trapFocus returnFocus withArrow>
          <Popover.Target>
            <UnstyledButton
              className={classes.teamIdentity}
              aria-label={`${rank?.name ?? '—'}: ${t('game.arena.team_options', 'Team details and token')}`}
            >
              <Avatar src={rank?.avatar} alt="" size={28} radius="sm">
                {rank?.name?.slice(0, 1) ?? 'T'}
              </Avatar>
              <Text fw={650} size="sm" className={classes.teamName}>
                {rank?.name ?? '—'}
              </Text>
              <Icon path={mdiChevronDown} size={0.7} aria-hidden="true" />
            </UnstyledButton>
          </Popover.Target>
          <Popover.Dropdown>
            <Stack gap="sm">
              <Text fw={650}>{rank?.name ?? '—'}</Text>
              <Text size="xs" c="dimmed">
                {division ?? t('game.arena.your_team', 'Your team')}
              </Text>
              {!archived && teamInfo?.teamToken && (
                <PasswordInput
                  label={t('team.label.token', 'Team token')}
                  description={t('team.content.token_copy_hint', 'Select the field to copy the token')}
                  value={teamInfo.teamToken}
                  readOnly
                  onClick={copyTeamToken}
                  onKeyDown={(event) => {
                    if (event.key === 'Enter') copyTeamToken()
                  }}
                />
              )}
              <Button component="a" href={`/games/${numId}/scoreboard`} variant="default" size="sm">
                {t('game.tab.scoreboard')}
              </Button>
              <Button
                component="a"
                href={`/games/${numId}/attack`}
                target="_blank"
                rel="noreferrer"
                variant="subtle"
                size="sm"
                leftSection={<Icon path={mdiSword} size={0.8} aria-hidden="true" />}
                rightSection={<Icon path={mdiOpenInNew} size={0.7} aria-hidden="true" />}
              >
                {t('game.button.attack')}
              </Button>
            </Stack>
          </Popover.Dropdown>
        </Popover>
        {hasJeopardy && (
          <dl className={classes.teamMetrics}>
            <div>
              <dt>{hasLive ? t('game.arena.jeopardy_rank', 'Jeopardy rank') : t('game.arena.rank', 'Rank')}</dt>
              <dd>{rank?.rank ? `#${rank.rank}` : '—'}</dd>
            </div>
            <div>
              <dt>{hasLive ? t('game.arena.jeopardy_score', 'Jeopardy score') : t('game.label.score_table.score')}</dt>
              <dd>{rank?.score?.toLocaleString() ?? '—'}</dd>
            </div>
            <div>
              <dt>{hasLive ? t('game.arena.jeopardy_solves', 'Jeopardy solves') : t('game.arena.solves', 'Solves')}</dt>
              <dd>{rank?.solvedCount ?? '—'}</dd>
            </div>
          </dl>
        )}
      </div>
    )
  }

  return (
    <Card {...props} shadow="sm">
      <Stack gap="xs">
        <Group gap="sm" wrap="nowrap">
          <Avatar imageProps={{ loading: 'lazy' }} alt={rank?.name ?? ''} size={50} radius="md" src={rank?.avatar}>
            {rank?.name?.slice(0, 1) ?? 'T'}
          </Avatar>
          <Skeleton visible={!rank}>
            <Stack gap={2} align="flex-start">
              <Title order={2} lineClamp={1} title={rank?.name ?? 'Team'}>
                {rank?.name ?? 'Team'}
              </Title>
              {division && (
                <Badge size="xs" variant="outline">
                  {division}
                </Badge>
              )}
            </Stack>
          </Skeleton>
        </Group>
        <Group grow ta="center">
          {item(t('game.label.score_table.rank_total'), rank?.rank || '-')}
          {division && item(t('game.label.score_table.rank_division'), rank?.divisionRank)}
          {item(t('game.label.score_table.score'), rank?.score)}
          {item(t('game.label.score_table.solved_count'), rank?.solvedCount)}
        </Group>
        <Progress
          value={solvedProgress}
          aria-label={t('game.label.score_table.solved_progress', 'Challenges solved')}
        />
        {!archived && (
          <PasswordInput
            label={t('team.label.token', 'Team token')}
            description={t('team.content.token_copy_hint', 'Select the field to copy the token')}
            value={teamInfo?.teamToken ?? ''}
            readOnly
            leftSection={<Icon path={mdiKey} size={1} />}
            onClick={copyTeamToken}
            onKeyDown={(event) => {
              if (event.key === 'Enter') copyTeamToken()
            }}
            classNames={{ innerInput: cx(misc.cCopy, misc.ffmono) }}
          />
        )}
      </Stack>
    </Card>
  )
}
