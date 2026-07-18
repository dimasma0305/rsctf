import {
  ActionIcon,
  Card,
  Checkbox,
  Group,
  Loader,
  Progress,
  Stack,
  Switch,
  Text,
  ThemeIcon,
  Tooltip,
  useMantineColorScheme,
  useMantineTheme,
} from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import {
  mdiAlertCircleOutline,
  mdiCheck,
  mdiCheckCircleOutline,
  mdiChessKing,
  mdiClockOutline,
  mdiCloseCircleOutline,
  mdiCogOutline,
  mdiDatabaseEditOutline,
  mdiFileAlertOutline,
  mdiFlagOutline,
  mdiHammerWrench,
  mdiMinusCircleOutline,
  mdiPuzzleEditOutline,
  mdiSwordCross,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { Dispatch, FC, SetStateAction, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useParams } from 'react-router'
import { useChallengeCategoryLabelMap, showErrorMsg } from '@Utils/Shared'
import api, { ChallengeInfoModel, ChallengeCategory } from '@Api'
import classes from '@Styles/ChallengeEditCard.module.css'

interface ChallengeEditCardProps {
  challenge: ChallengeInfoModel
  onToggle: (challenge: ChallengeInfoModel, setDisabled: Dispatch<SetStateAction<boolean>>) => void
  /**
   * Revalidate the parent challenge list. The card calls this after
   * triggering a Build so the badge + icon transition through
   * Queued → Building → Success/Failed without the operator having to
   * reload the page. Used by a polling effect while a build is in
   * flight; the parent list otherwise uses OnceSWRConfig and never
   * refreshes on its own.
   */
  onMutate?: () => void
  /** When set, the card shows a selection checkbox (for batch actions). */
  selectable?: boolean
  selected?: boolean
  onSelectChange?: (checked: boolean) => void
}

export const ChallengeEditCard: FC<ChallengeEditCardProps> = ({
  challenge,
  onToggle,
  onMutate,
  selectable,
  selected,
  onSelectChange,
}) => {
  const challengeCategoryLabelMap = useChallengeCategoryLabelMap()
  // Fall back to Misc when the challenge's category isn't in the map (e.g. an
  // imported game carrying an unknown/legacy category): without this the
  // `data!.icon` below threw "Cannot read properties of undefined (reading 'icon')"
  // and crashed the whole admin challenges page.
  const data =
    challengeCategoryLabelMap.get(challenge.category as ChallengeCategory) ??
    challengeCategoryLabelMap.get(ChallengeCategory.Misc)
  const theme = useMantineTheme()
  const { id } = useParams()

  const [disabled, setDisabled] = useState(false)
  const [building, setBuilding] = useState(false)

  const { t } = useTranslation()
  const numId = parseInt(id ?? '-1')

  const inFlightBuild = challenge.buildStatus === 'Queued' || challenge.buildStatus === 'Building'

  // Only Container-type challenges can have a local Dockerfile to
  // build. Static/Dynamic Attachment challenges are file-only; showing
  // a Build button there is just noise. Same for challenges that
  // explicitly ship a registry image (NotApplicable).
  const isBuildable =
    (challenge.type === 'StaticContainer' ||
      challenge.type === 'DynamicContainer' ||
      challenge.type === 'AttackDefense' ||
      challenge.type === 'KingOfTheHill') &&
    challenge.buildStatus !== 'NotApplicable'

  const onBuildNow = async () => {
    if (challenge.id == null) return
    setBuilding(true)
    try {
      await api.edit.editRebuildChallengeImage(numId, challenge.id)
      showNotification({
        color: 'teal',
        message: t('admin.notification.builds.enqueued'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      // Kick the parent list to revalidate so the badge / icon
      // reflect the new Queued status without a manual reload.
      onMutate?.()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setBuilding(false)
    }
  }

  // Poll while a build is in flight so the parent list refreshes
  // every 2s — mirrors the cadence used by the challenge-edit page's
  // inline build-log section. Stops as soon as the status leaves
  // Queued/Building.
  useEffect(() => {
    if (!inFlightBuild || !onMutate) return
    const timer = window.setInterval(() => {
      onMutate()
    }, 2000)
    return () => window.clearInterval(timer)
  }, [inFlightBuild, onMutate])
  const { colorScheme } = useMantineColorScheme()

  const color = data?.color ?? theme.primaryColor
  const colors = theme.colors[color]

  const minIdx = colorScheme === 'dark' ? 8 : 6
  const curIdx = colorScheme === 'dark' ? 6 : 4

  const [min, cur, tot] = [challenge.minScore ?? 0, challenge.score ?? 500, challenge.originalScore ?? 500]
  const minRate = (min / tot) * 100
  const curRate = (cur / tot) * 100

  return (
    <Card shadow="sm" p="sm" pos="relative" style={{ overflow: 'hidden' }}>
      {/* Category (Web/Pwn/…) rendered as a large faded watermark behind the
          row, instead of a small icon beside the enable toggle. */}
      <Icon
        path={data?.icon ?? mdiFlagOutline}
        color={theme.colors[data?.color ?? theme.primaryColor][5]}
        size={3.5}
        style={{
          position: 'absolute',
          right: '-0.5rem',
          top: '50%',
          transform: 'translateY(-50%)',
          opacity: colorScheme === 'dark' ? 0.1 : 0.07,
          pointerEvents: 'none',
          zIndex: 0,
        }}
      />
      {/* No space-between: the slider + checkbox stay pinned left and the buttons
          right, with the content Stack growing to fill between them — so the
          slider never shifts when the content (icons, build button) varies. */}
      <Group wrap="wrap" gap="xs" pos="relative" align="center" style={{ zIndex: 1 }}>
        {selectable && (
          <Checkbox
            size="sm"
            checked={!!selected}
            onChange={(e) => onSelectChange?.(e.currentTarget.checked)}
            aria-label={t('admin.button.challenges.select')}
          />
        )}
        <Switch
          color={color}
          disabled={disabled}
          checked={challenge.isEnabled}
          onChange={() => onToggle(challenge, setDisabled)}
        />

        <Stack gap={0} style={{ flex: '1 1 12rem', minWidth: 0 }}>
          <Group gap={6} wrap="nowrap">
            <Text truncate fw="bold" style={{ flex: 1, minWidth: 0 }}>
              {challenge.title}
            </Text>
            {/* Status was text badges (A&D, Built, …); now fixed-size icon chips in
                a flex-shrink:0 group pinned to the right of the flex:1 title. Same
                width regardless of state, so build-status transitions
                (Queued→Building→Built) no longer shift the row's layout. Hover for
                the label. */}
            <Group gap={4} wrap="wrap" style={{ flexShrink: 0 }}>
              {challenge.type === 'AttackDefense' && (
                <Tooltip label={t('admin.content.review.badge.attack_defense_help')} multiline w={260}>
                  <ThemeIcon size="sm" radius="sm" color="red" variant="filled">
                    <Icon path={mdiSwordCross} size={0.7} />
                  </ThemeIcon>
                </Tooltip>
              )}
              {challenge.type === 'KingOfTheHill' && (
                <Tooltip
                  label={t(
                    'admin.content.review.badge.koth_help',
                    'King of the Hill — single shared hill, hold-time scoring'
                  )}
                  multiline
                  w={260}
                >
                  <ThemeIcon size="sm" radius="sm" color="violet" variant="filled">
                    <Icon path={mdiChessKing} size={0.7} />
                  </ThemeIcon>
                </Tooltip>
              )}
              {challenge.reviewStatus === 'Pending' && (
                <Tooltip label={t('admin.content.review.badge.pending')}>
                  <ThemeIcon size="sm" radius="sm" color="yellow" variant="filled">
                    <Icon path={mdiClockOutline} size={0.7} />
                  </ThemeIcon>
                </Tooltip>
              )}
              {challenge.reviewStatus === 'Rejected' && (
                <Tooltip label={t('admin.content.review.badge.rejected')}>
                  <ThemeIcon size="sm" radius="sm" color="red" variant="filled">
                    <Icon path={mdiCloseCircleOutline} size={0.7} />
                  </ThemeIcon>
                </Tooltip>
              )}
              {challenge.buildStatus === 'Queued' && (
                <Tooltip label={t('admin.content.review.badge.queued')}>
                  <ThemeIcon size="sm" radius="sm" color="blue" variant="light">
                    <Icon path={mdiClockOutline} size={0.7} />
                  </ThemeIcon>
                </Tooltip>
              )}
              {challenge.buildStatus === 'Building' && (
                <Tooltip label={t('admin.content.review.badge.building')}>
                  <ThemeIcon size="sm" radius="sm" color="yellow" variant="light">
                    <Icon path={mdiCogOutline} size={0.7} />
                  </ThemeIcon>
                </Tooltip>
              )}
              {challenge.buildStatus === 'Success' && (
                <Tooltip label={t('admin.content.review.badge.built')}>
                  <ThemeIcon size="sm" radius="sm" color="teal" variant="light">
                    <Icon path={mdiCheckCircleOutline} size={0.7} />
                  </ThemeIcon>
                </Tooltip>
              )}
              {challenge.buildStatus === 'NotApplicable' && (
                <Tooltip label={t('admin.content.review.badge.not_applicable_help')} multiline w={240}>
                  <ThemeIcon size="sm" radius="sm" color="gray" variant="light">
                    <Icon path={mdiMinusCircleOutline} size={0.7} />
                  </ThemeIcon>
                </Tooltip>
              )}
              {challenge.buildStatus === 'MissingDockerfile' && (
                <Tooltip label={t('admin.content.review.badge.missing_dockerfile_help')} multiline w={260}>
                  <ThemeIcon size="sm" radius="sm" color="orange" variant="light">
                    <Icon path={mdiFileAlertOutline} size={0.7} />
                  </ThemeIcon>
                </Tooltip>
              )}
              {challenge.buildStatus === 'Failed' && (
                <Tooltip label={t('admin.content.review.badge.build_failed_help')} multiline w={240}>
                  <ThemeIcon size="sm" radius="sm" color="red" variant="filled">
                    <Icon path={mdiAlertCircleOutline} size={0.7} />
                  </ThemeIcon>
                </Tooltip>
              )}
            </Group>
          </Group>
          <Text size="sm" fw="bold" ff="monospace" w="5rem">
            {challenge.score}
            <Text span fw="bold" c="dimmed">
              /{challenge.originalScore}pts
            </Text>
          </Text>
        </Stack>

        <Group gap={4} wrap="nowrap" ml="auto">
          {isBuildable && (
            <Tooltip
              label={
                inFlightBuild ? t('admin.button.challenges.build_in_flight') : t('admin.button.challenges.build_now')
              }
              ta="end"
              position="left"
              offset={98}
              classNames={classes}
            >
              <ActionIcon
                c={color}
                variant="subtle"
                disabled={building || inFlightBuild}
                aria-label={
                  inFlightBuild ? t('admin.button.challenges.build_in_flight') : t('admin.button.challenges.build_now')
                }
                onClick={onBuildNow}
              >
                {building || inFlightBuild ? <Loader size="xs" /> : <Icon path={mdiHammerWrench} size={1} />}
              </ActionIcon>
            </Tooltip>
          )}
          <Tooltip label={t('admin.button.challenges.edit')} position="left" offset={10} classNames={classes}>
            <ActionIcon
              c={color}
              component={Link}
              to={`/admin/games/${id}/challenges/${challenge.id}`}
              aria-label={t('admin.button.challenges.edit')}
            >
              <Icon path={mdiPuzzleEditOutline} size={1} />
            </ActionIcon>
          </Tooltip>
          <Tooltip
            label={t('admin.button.challenges.edit_more')}
            ta="end"
            position="left"
            offset={54}
            classNames={classes}
          >
            <ActionIcon
              c={color}
              component={Link}
              to={`/admin/games/${id}/challenges/${challenge.id}/flags`}
              aria-label={t('admin.button.challenges.edit_more')}
            >
              <Icon path={mdiDatabaseEditOutline} size={1} />
            </ActionIcon>
          </Tooltip>
        </Group>
      </Group>

      <Card.Section mt="sm" pos="relative" style={{ zIndex: 1 }}>
        <Progress.Root radius={0}>
          <Progress.Section value={minRate} color={colors[minIdx]} />
          <Progress.Section value={curRate - minRate} color={colors[curIdx]} />
        </Progress.Root>
      </Card.Section>
    </Card>
  )
}
