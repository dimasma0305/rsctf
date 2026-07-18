import {
  Badge,
  Box,
  Card,
  Center,
  Code,
  Divider,
  Group,
  Stack,
  Text,
  Title,
  Tooltip,
  alpha,
  useMantineColorScheme,
  useMantineTheme,
} from '@mantine/core'
import { mdiCrown, mdiFlag, mdiFlagOutline, mdiSwordCross, mdiThumbUp } from '@mdi/js'
import { Icon } from '@mdi/react'
import cx from 'clsx'
import dayjs from 'dayjs'
import { FC, useMemo } from 'react'
import { Trans, useTranslation } from 'react-i18next'
import { ScrollingText } from '@Components/ScrollingText'
import { useLanguage } from '@Utils/I18n'
import { BloodsTypes, PartialIconProps, useChallengeCategoryLabelMap } from '@Utils/Shared'
import { ChallengeInfo, ChallengeType, SubmissionType } from '@Api'
import classes from '@Styles/ChallengeCard.module.css'
import misc from '@Styles/Misc.module.css'

interface ChallengeCardProps {
  challenge: ChallengeInfo
  solved?: boolean
  onClick?: () => void
  iconMap: Map<SubmissionType, PartialIconProps | undefined>
  colorMap: Map<SubmissionType, string | undefined>
  teamId?: number
  rating?: { likes: number; dislikes: number }
}

export const ChallengeCard: FC<ChallengeCardProps> = (props: ChallengeCardProps) => {
  const { challenge, solved, onClick, iconMap, teamId, colorMap, rating } = props
  const challengeCategoryLabelMap = useChallengeCategoryLabelMap()
  const cateData = challengeCategoryLabelMap.get(challenge.category!)
  const theme = useMantineTheme()
  const { colorScheme } = useMantineColorScheme()
  const { locale } = useLanguage()
  const { t } = useTranslation()
  // A&D AND KotH both run on the live-scoring engine — neither has a static
  // "challenge.score" worth showing. Without including KotH here, the card
  // would print the default OriginalScore (e.g. "100 pts") which is meaningless
  // for a hill (hold-credit scored, not first-blood scored).
  // "AD engine" = both AttackDefense and KingOfTheHill — they share the
  // live-scoring branch (no static challenge.score). `isAttackDefense` is
  // kept separate from `isKoth` for the one place that needs the strict
  // AttackDefense distinction (the top-right sword vs crown badge).
  const isAdEngine = challenge.type === ChallengeType.AttackDefense || challenge.type === ChallengeType.KingOfTheHill
  const isKoth = challenge.type === ChallengeType.KingOfTheHill
  const isAttackDefense = challenge.type === ChallengeType.AttackDefense

  const isFaded = useMemo(() => {
    if (!challenge.deadline) return false

    return dayjs().isAfter(dayjs(challenge.deadline))
  }, [challenge.deadline])

  const ratingBadge = useMemo(() => {
    if (!rating) return null
    const total = rating.likes + rating.dislikes
    if (total < 3) return null
    const pct = Math.round((rating.likes / total) * 100)
    const color = pct >= 70 ? 'teal' : pct >= 40 ? 'orange' : 'red'
    return { pct, color }
  }, [rating])

  return (
    <Card
      component="article"
      onClick={onClick}
      shadow="sm"
      className={cx(misc.hoverCard, classes.root)}
      data-faded={solved || isFaded || undefined}
      data-no-move
    >
      <button
        type="button"
        className={classes.keyboardAction}
        onClick={(event) => {
          event.stopPropagation()
          onClick?.()
        }}
      >
        {t('challenge.button.open', 'Open challenge: {{title}}', { title: challenge.title })}
      </button>
      <Stack gap="xs" pos="relative" style={{ zIndex: 99 }}>
        <Group h="30px" wrap="nowrap" justify="space-between" gap={2}>
          <Group gap={6} wrap="nowrap" style={{ flex: 1, minWidth: 0 }}>
            {/* Category icon at the top-left, matching the ChallengeModal header
                pattern (`[category icon] [title] [pts/LIVE]`). For jeopardy this
                is the Web/Pwn/Crypto/Misc tier color; for A&D / KotH it's still
                the per-challenge category (organizers tag hills too). Without
                this the card had only the engine badge on the right — the modal
                showed a colored category icon at top-left and players opening
                a card expected the same visual cue on the tile. */}
            {cateData && (
              <Icon
                path={cateData.icon}
                size={0.9}
                color={theme.colors[cateData.color][colorScheme === 'dark' ? 4 : 6]}
                style={{ flexShrink: 0 }}
              />
            )}
            <ScrollingText text={challenge.title || ''} size="lg" />
          </Group>
          <Group gap={4} wrap="nowrap">
            {/* Engine badge — same slot for all three so the icon position is
                stable across challenges; color matches the kind-switcher +
                section dividers so the three are visually consistent. */}
            {!isAdEngine && (
              <Tooltip
                label={t('challenge.tooltip.jeopardy_card', 'Jeopardy — submit the flag once for points')}
                position="top"
                withArrow
              >
                <Icon path={mdiFlagOutline} size={0.7} color="var(--mantine-color-blue-6)" />
              </Tooltip>
            )}
            {isAttackDefense && (
              <Tooltip
                label={t('challenge.tooltip.ad_card', 'Attack & Defense — live scoring, submit via API')}
                position="top"
                withArrow
              >
                <Icon path={mdiSwordCross} size={0.7} color="var(--mantine-color-red-6)" />
              </Tooltip>
            )}
            {isKoth && (
              <Tooltip
                label={t('challenge.tooltip.koth_card', 'King of the Hill — hold the marker to score')}
                position="top"
                withArrow
              >
                <Icon path={mdiCrown} size={0.7} color="var(--mantine-color-violet-6)" />
              </Tooltip>
            )}
            {ratingBadge && (
              <Tooltip label={`${rating!.likes}👍 / ${rating!.dislikes}👎`} position="top" withArrow>
                <Badge
                  size="xs"
                  color={ratingBadge.color}
                  variant="light"
                  leftSection={<Icon path={mdiThumbUp} size={0.5} />}
                  style={{ flexShrink: 0, cursor: 'default' }}
                >
                  {ratingBadge.pct}%
                </Badge>
              </Tooltip>
            )}
          </Group>
        </Group>
        <Divider size="sm" color={cateData?.color} />
        <Group wrap="nowrap" justify={isAdEngine ? 'center' : 'space-between'} align="center" gap={2}>
          {!isAdEngine && (
            <Text ta="center" fw="bold" fz="lg" ff="monospace">
              {challenge.score}&nbsp;pts
            </Text>
          )}
          <Stack gap="xs">
            {isAdEngine ? (
              <Title order={6} ta="center" mt={`calc(${theme.spacing.xs} / 2)`} c="dimmed">
                {isKoth
                  ? t('challenge.content.koth_live_caption', 'Per-tick hold scoring')
                  : t('challenge.content.ad_live_caption', 'Per-round scoring')}
              </Title>
            ) : (
              <Title order={6} ta="center" mt={`calc(${theme.spacing.xs} / 2)`}>
                <Trans
                  i18nKey={'challenge.content.solved'}
                  values={{
                    solved: challenge.solved,
                  }}
                >
                  _
                  <Code fz="sm" fw="bolder" bg="transparent">
                    _
                  </Code>
                  _
                </Trans>
              </Title>
            )}
            <Group justify="center" gap="md" h={20} wrap="nowrap">
              {challenge.bloods &&
                challenge.bloods.map((blood, idx) => {
                  const iconProps = iconMap.get(BloodsTypes[idx])!
                  return (
                    <Tooltip.Floating
                      key={idx}
                      position="bottom"
                      multiline
                      label={
                        <Stack gap={0}>
                          <Text fw={500} size="sm">
                            {blood?.name}
                          </Text>
                          <Text fw={500} size="xs" c="dimmed">
                            {dayjs(blood?.submitTimeUtc).locale(locale).format('SLL LTS')}
                          </Text>
                        </Stack>
                      }
                    >
                      <div style={{ position: 'relative', height: 20 }}>
                        <div className={classes.blood}>
                          <Icon {...iconProps} />
                        </div>
                        <Box
                          className={classes.spike}
                          data-blood={teamId === blood?.id || undefined}
                          __vars={{
                            '--blood-color': colorMap.get(BloodsTypes[idx]),
                          }}
                        />
                      </div>
                    </Tooltip.Floating>
                  )
                })}
            </Group>
          </Stack>
        </Group>
      </Stack>
      {/* Big category watermark icon (the card's "visualizer") — identical for
          all three engines: jeopardy, A&D and KotH all use the per-challenge
          category icon + color, exactly like the jeopardy card always has. The
          only per-engine cue is the top-right badge (flag / sword / crown). */}
      {cateData && (
        <Icon
          size={4}
          path={cateData.icon}
          color={alpha(theme.colors[cateData.color][7], 0.3)}
          className={classes.icon}
        />
      )}
      {solved && (
        <Center className={classes.flag}>
          <Icon size={1} path={mdiFlag} />
        </Center>
      )}
    </Card>
  )
}
