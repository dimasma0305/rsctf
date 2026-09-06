import { Badge, Card, Group, Stack, Text, Tooltip } from '@mantine/core'
import { mdiCheckCircleOutline, mdiClockOutline, mdiCrown, mdiFlagOutline, mdiSwordCross, mdiThumbUp } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { useLanguage } from '@Utils/I18n'
import { useServerNow } from '@Utils/ServerClock'
import { BloodsTypes, PartialIconProps, useChallengeCategoryLabelMap } from '@Utils/Shared'
import { ChallengeInfo, ChallengeType, SubmissionType } from '@Api'
import classes from '@Styles/ChallengeCard.module.css'

interface ChallengeCardProps {
  challenge: ChallengeInfo
  solved?: boolean
  onClick?: () => void
  contextLabel?: string
  iconMap: Map<SubmissionType, PartialIconProps | undefined>
  colorMap: Map<SubmissionType, string | undefined>
  teamId?: number
  rating?: { likes: number; dislikes: number }
}

const ChallengeCardContent: FC<ChallengeCardProps & { deadlinePassed: boolean }> = ({
  challenge,
  solved,
  onClick,
  contextLabel,
  iconMap,
  teamId,
  rating,
  deadlinePassed,
}) => {
  const { t } = useTranslation()
  const { locale } = useLanguage()
  const category = useChallengeCategoryLabelMap().get(challenge.category!)
  const isKoth = challenge.type === ChallengeType.KingOfTheHill
  const isAd = challenge.type === ChallengeType.AttackDefense
  const liveScoring = isKoth || isAd
  const mode = isKoth ? 'King of the Hill' : isAd ? 'Attack & Defense' : 'Jeopardy'
  const modeIcon = isKoth ? mdiCrown : isAd ? mdiSwordCross : mdiFlagOutline
  const ratingTotal = (rating?.likes ?? 0) + (rating?.dislikes ?? 0)

  return (
    <Card
      component="article"
      className={classes.root}
      data-faded={solved || deadlinePassed || undefined}
      data-solved={solved || undefined}
      data-guide="challenge-card"
      data-no-move
    >
      <Group justify="space-between" gap="xs" wrap="wrap">
        <Text size="xs" c="dimmed" fw={600} className={classes.category}>
          {category && <Icon path={category.icon} size={0.75} aria-hidden="true" />}
          {category?.name ?? challenge.category}
        </Text>
        {solved ? (
          <Badge
            color="teal"
            variant="light"
            leftSection={<Icon path={mdiCheckCircleOutline} size={0.65} aria-hidden="true" />}
          >
            {t('common.workspace.solved', 'Solved')}
          </Badge>
        ) : deadlinePassed ? (
          <Badge
            color="gray"
            variant="light"
            leftSection={<Icon path={mdiClockOutline} size={0.65} aria-hidden="true" />}
          >
            {t('common.workspace.closed', 'Closed')}
          </Badge>
        ) : (
          <Text size="xs" c="dimmed">
            #{challenge.id}
          </Text>
        )}
      </Group>
      <button
        type="button"
        onClick={onClick}
        className={classes.openButton}
        aria-label={t('challenge.button.open', 'Open challenge: {{title}}', { title: challenge.title })}
      >
        {challenge.title}
      </button>
      {contextLabel && (
        <Text size="xs" c="dimmed" className={classes.context}>
          {contextLabel}
        </Text>
      )}
      <Group justify="space-between" gap="sm" className={classes.scoreRow}>
        <Stack gap={1}>
          <Text className={classes.score}>
            {liveScoring ? t('common.workspace.live_scoring', 'Live scoring') : challenge.score?.toLocaleString()}
          </Text>
          <Text size="xs" c="dimmed">
            {liveScoring
              ? t('common.workspace.continuous_scoring', 'Scored during play')
              : t('common.workspace.points', 'points')}
          </Text>
        </Stack>
        {!liveScoring && (
          <Text size="xs" c="dimmed">
            {t('common.workspace.solves', '{{count}} solves', { count: challenge.solved ?? 0 })}
          </Text>
        )}
      </Group>
      <div className={classes.footer}>
        <Text size="xs" c="dimmed" className={classes.category}>
          <Icon path={modeIcon} size={0.7} aria-hidden="true" />
          {mode}
        </Text>
        {rating && ratingTotal >= 3 && (
          <Text size="xs" c="dimmed" className={classes.category} title={`${rating.likes} / ${ratingTotal}`}>
            <Icon path={mdiThumbUp} size={0.65} aria-hidden="true" />
            {Math.round((rating.likes / ratingTotal) * 100)}%
          </Text>
        )}
      </div>
      {!!challenge.bloods?.length && (
        <Group gap="xs" aria-label={t('common.workspace.first_solves', 'First solves')} className={classes.bloods}>
          {challenge.bloods.slice(0, 3).map((blood, index) => {
            const icon = iconMap.get(BloodsTypes[index])
            const label = `${index + 1}. ${blood.name} · ${dayjs(blood.submitTimeUtc).locale(locale).format('L LTS')}`
            return (
              <Tooltip key={index} label={label} events={{ hover: true, focus: true, touch: false }}>
                <span
                  tabIndex={0}
                  aria-label={label}
                  data-own={teamId === blood.id || undefined}
                  className={classes.blood}
                >
                  {icon && <Icon {...icon} aria-hidden="true" />}
                  <span>{blood.name}</span>
                </span>
              </Tooltip>
            )
          })}
        </Group>
      )}
    </Card>
  )
}

const DeadlineAwareChallengeCard: FC<ChallengeCardProps> = (props) => {
  const now = useServerNow()
  return <ChallengeCardContent {...props} deadlinePassed={now.isAfter(dayjs(props.challenge.deadline))} />
}

export const ChallengeCard: FC<ChallengeCardProps> = (props) =>
  props.challenge.deadline ? (
    <DeadlineAwareChallengeCard {...props} />
  ) : (
    <ChallengeCardContent {...props} deadlinePassed={false} />
  )
