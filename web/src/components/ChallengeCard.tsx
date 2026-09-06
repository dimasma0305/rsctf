import { Card, Group, Text, Tooltip } from '@mantine/core'
import { mdiCheckCircleOutline, mdiClockOutline, mdiCrown, mdiFlagOutline, mdiSwordCross, mdiThumbUp } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import { useLanguage } from '@Utils/I18n'
import { useServerNow } from '@Utils/ServerClock'
import { BloodsTypes, PartialIconProps, useChallengeCategoryLabelMap } from '@Utils/Shared'
import { buildSemanticAccentColors } from '@Utils/ThemeContrast'
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
  const category = useChallengeCategoryLabelMap().get(challenge.category)
  const categoryColor = category?.colors[6] ?? '#228be6'
  const accent = useMemo(() => buildSemanticAccentColors(categoryColor), [categoryColor])
  const isKoth = challenge.type === ChallengeType.KingOfTheHill
  const isAd = challenge.type === ChallengeType.AttackDefense
  const liveScoring = isKoth || isAd
  const mode = isKoth ? 'King of the Hill' : isAd ? 'Attack & Defense' : 'Jeopardy'
  const modeIcon = isKoth ? mdiCrown : isAd ? mdiSwordCross : mdiFlagOutline
  const categoryIcon = category?.icon ?? modeIcon
  const ratingTotal = (rating?.likes ?? 0) + (rating?.dislikes ?? 0)

  return (
    <Card
      component="article"
      className={classes.root}
      data-faded={solved || deadlinePassed || undefined}
      data-solved={solved || undefined}
      data-state={solved ? 'solved' : deadlinePassed ? 'closed' : 'open'}
      data-guide="challenge-card"
      data-no-move
      __vars={{
        '--card-category-light': accent[0],
        '--card-category-dark': accent[1],
        '--card-category-art': categoryColor,
      }}
    >
      <Icon path={categoryIcon} className={classes.watermark} aria-hidden="true" />
      <div className={classes.header}>
        <Icon path={categoryIcon} size={1.2} className={classes.categoryIcon} aria-hidden="true" />
        <button
          type="button"
          onClick={onClick}
          className={classes.openButton}
          aria-label={t('challenge.button.open', 'Open challenge: {{title}}', { title: challenge.title })}
          aria-haspopup="dialog"
        >
          {challenge.title}
        </button>
      </div>
      <div className={classes.metadata}>
        <div className={classes.kind}>
          <span className={classes.category}>{category?.name ?? challenge.category}</span>
          <span className={classes.mode}>{mode}</span>
        </div>
        {(solved || deadlinePassed) && (
          <span className={classes.status}>
            <Icon path={solved ? mdiCheckCircleOutline : mdiClockOutline} size={0.7} aria-hidden="true" />
            {solved ? t('common.workspace.solved', 'Solved') : t('common.workspace.closed', 'Closed')}
          </span>
        )}
      </div>
      {contextLabel && (
        <Text size="xs" c="dimmed" className={classes.context}>
          {contextLabel}
        </Text>
      )}
      {liveScoring ? (
        <div className={classes.liveScore}>
          <Text fw={600}>{t('common.workspace.live_scoring', 'Live scoring')}</Text>
          <Text size="xs" c="dimmed">
            {t('common.workspace.continuous_scoring', 'Scored during play')}
          </Text>
        </div>
      ) : (
        <dl className={classes.metrics}>
          <div className={classes.points}>
            <dt>{t('common.cards.points_short', 'pts')}</dt>
            <dd>{challenge.score?.toLocaleString(locale) ?? '—'}</dd>
          </div>
          <div>
            <dt>{t('common.cards.solves', 'solves')}</dt>
            <dd>{challenge.solved?.toLocaleString(locale) ?? '—'}</dd>
          </div>
        </dl>
      )}
      {rating && ratingTotal >= 3 && (
        <Text size="xs" c="dimmed" className={classes.rating} title={`${rating.likes} / ${ratingTotal}`}>
          <Icon path={mdiThumbUp} size={0.65} aria-hidden="true" />
          {Math.round((rating.likes / ratingTotal) * 100)}%
        </Text>
      )}
      {!!challenge.bloods?.length && (
        <div className={classes.bloods}>
          <Text size="xs" c="dimmed" mb={4}>
            {t('common.workspace.first_solves', 'First solves')}
          </Text>
          <Group gap="xs">
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
        </div>
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
