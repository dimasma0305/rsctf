import {
  ActionIcon,
  Alert,
  Avatar,
  Badge,
  Box,
  Center,
  Divider,
  Group,
  Loader,
  Modal,
  Paper,
  SimpleGrid,
  Stack,
  Table,
  Text,
  Tooltip,
  UnstyledButton,
  useMantineColorScheme,
  useMantineTheme,
} from '@mantine/core'
import {
  mdiAlertCircleOutline,
  mdiClockOutline,
  mdiCounter,
  mdiCrown,
  mdiFlagVariantOutline,
  mdiInformationOutline,
  mdiTimerSandComplete,
  mdiTrophyOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import cx from 'clsx'
import dayjs from 'dayjs'
import { FC, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import {
  AdLikeCategoryHeaderRow,
  AdLikeHiddenCols,
  AdLikePinnedHeaderCells,
  AdLikePinnedRowCells,
  AdLikeStatusLegend,
  AdLikeToolbar,
  ITEM_COUNT_PER_PAGE,
  adLikeRowHighlight,
  fmtPts,
  statusBg,
  statusColor,
  useAdLikeScoreboardState,
} from '@Components/AdLikeScoreboard'
import { ScoreboardPagination } from '@Components/ScoreboardPagination'
import { useIsMobile } from '@Utils/ThemeOverride'
import { epochProgress } from '@Utils/epochProgress'
import { isKothResetTransition, kothConfirmationProgress, maxKothCooldownTicks } from '@Utils/kothLifecycle'
import {
  type KothHillScore,
  type KothScoreboardModel,
  type KothScoreboardHill,
  type KothTeamScoreRow,
  useGame,
} from '@Hooks/useGame'
import misc from '@Styles/Misc.module.css'
import classes from '@Styles/ScoreboardTable.module.css'

// Matches the A&D board: one score column followed by three evidence columns.
const SUBCOL = { score: 76, acquisition: 58, control: 58, sla: 54 }
const GROUP_W = SUBCOL.score + SUBCOL.acquisition + SUBCOL.control + SUBCOL.sla

const clampRate = (rate: number) => Math.max(0, Math.min(1, rate))
const formatPercent = (rate: number) => `${(clampRate(rate) * 100).toFixed(1)}%`
const formatFormulaNumber = (value: number) => (Number.isFinite(value) ? value.toFixed(2).replace(/0$/, '') : '—')
const hasEpochBasis = (points: number, weight: number) =>
  Number.isFinite(points) && Number.isFinite(weight) && weight > 0
const hasProjection = (settled: number, projected: number) => Math.abs(settled - projected) > 0.05
const readableMetricColor = (color: string, dark: boolean) => `${color}.${dark ? 4 : 9}`
const acquisitionWindowCount = (team: KothTeamScoreRow) =>
  team.hills.reduce((total, hill) => total + hill.acquisitionWindows, 0)

interface CompactMetricProps {
  label: string
  value: string
  color?: string
}

const CompactMetric: FC<CompactMetricProps> = ({ label, value, color }) => {
  const { colorScheme } = useMantineColorScheme()
  return (
    <Stack gap={1} ta="center">
      <Text
        size="sm"
        fw={800}
        c={color ? readableMetricColor(color, colorScheme === 'dark') : undefined}
        className={misc.ffmono}
      >
        {value}
      </Text>
      <Text size="xs" c="dimmed">
        {label}
      </Text>
    </Stack>
  )
}

const KothLifecycleBadges: FC<{
  hill: KothScoreboardHill
  cycleTicks: number
  confirmationTicks: number
}> = ({ hill, cycleTicks, confirmationTicks }) => {
  const { t } = useTranslation()
  const phase = hill.resetPhase
  const isTransition = hill.cycleNumber > 0 && isKothResetTransition(phase)
  const isApiArena = hill.claimSource === 'Api'
  const cooldown = hill.cooldownParticipants
  const [confirmationCurrent, confirmationRequired] = kothConfirmationProgress(
    hill.provisionalConfirmationTicks,
    confirmationTicks
  )

  if (isApiArena) {
    return (
      <Group gap={4} justify="center" wrap="wrap">
        <Badge size="xs" color="blue" variant="light">
          {t('game.content.scoreboard.koth.api.health_supervised', 'Persistent · health supervised')}
        </Badge>
        {isTransition && (
          <Badge size="xs" color={phase === 'Failed' ? 'red' : 'orange'} variant="filled">
            {t('game.content.scoreboard.koth.api.recovery_phase', {
              phase,
              defaultValue: 'Recovery: {{phase}}',
            })}
          </Badge>
        )}
      </Group>
    )
  }

  if (
    hill.cycleNumber <= 0 &&
    !hill.provisionalClaimantTeamName &&
    !isTransition &&
    hill.nextResetTicks == null &&
    cooldown.length === 0
  ) {
    return null
  }

  return (
    <Group gap={4} justify="center" wrap="wrap">
      {hill.cycleNumber > 0 && (
        <Badge size="xs" color="violet" variant="light">
          C{hill.cycleNumber}
          {` · ${hill.cycleTick}/${cycleTicks}`}
        </Badge>
      )}
      {isTransition ? (
        <Badge size="xs" color={phase === 'Failed' ? 'red' : 'orange'} variant="filled">
          {t('game.content.scoreboard.koth.reset_phase', {
            phase,
            defaultValue: 'Reset: {{phase}}',
          })}
        </Badge>
      ) : hill.nextResetTicks != null ? (
        <Badge size="xs" color="gray" variant="light">
          {t('game.content.scoreboard.koth.reset_in_short', {
            count: hill.nextResetTicks,
            defaultValue: 'Reset {{count}}t',
          })}
        </Badge>
      ) : null}
      {hill.provisionalClaimantTeamName && (
        <Tooltip
          label={t('game.content.scoreboard.koth.provisional_detail', {
            team: hill.provisionalClaimantTeamName,
            current: confirmationCurrent,
            required: confirmationRequired,
            defaultValue: '{{team}} is provisional ({{current}}/{{required}} healthy control ticks).',
          })}
          withinPortal
        >
          <Badge size="xs" color="orange" variant="light">
            {t('game.content.scoreboard.koth.provisional_short', {
              team: hill.provisionalClaimantTeamName,
              current: confirmationCurrent,
              required: confirmationRequired,
              defaultValue: 'Provisional {{team}} · {{current}}/{{required}}',
            })}
          </Badge>
        </Tooltip>
      )}
      {cooldown.length > 0 && (
        <Tooltip
          label={t('game.content.scoreboard.koth.cooldown_detail', {
            teams: cooldown.map((entry) => entry.teamName).join(', '),
            count: maxKothCooldownTicks(cooldown),
            defaultValue: 'Champion cooldown: {{teams}} · {{count}} tick(s) remaining.',
          })}
          withinPortal
        >
          <Badge size="xs" color="grape" variant="light">
            {t('game.content.scoreboard.koth.cooldown_short', {
              count: maxKothCooldownTicks(cooldown),
              defaultValue: 'Cooldown {{count}}t',
            })}
          </Badge>
        </Tooltip>
      )}
    </Group>
  )
}

interface HillCardProps {
  hill: KothScoreboardHill
  score?: KothHillScore
  cycleTicks: number
  confirmationTicks: number
}

const HillCard: FC<HillCardProps> = ({ hill, score, cycleTicks, confirmationTicks }) => {
  const { t } = useTranslation()
  const { colorScheme } = useMantineColorScheme()
  const dark = colorScheme === 'dark'
  const status = hill.lastCheckStatus
  const healthyTicks = score?.healthyResponsibleTicks ?? 0
  const isApiArena = hill.claimSource === 'Api'
  const firstMetric = isApiArena
    ? t('game.content.scoreboard.koth.api.activity', 'Completion')
    : t('game.content.scoreboard.koth.epoch.column.acquisition', 'Acquisition')
  const secondMetric = isApiArena
    ? t('game.content.scoreboard.koth.api.objective', 'Objective')
    : t('game.content.scoreboard.koth.epoch.column.control', 'Control')
  const thirdMetric = isApiArena
    ? t('game.content.scoreboard.koth.api.integrity', 'Crown share')
    : t('game.content.scoreboard.koth.epoch.column.reliability', 'Reliability')

  return (
    <Paper withBorder p="sm" radius="md">
      <Stack gap="sm">
        <Group justify="space-between" gap="xs" wrap="nowrap">
          <Stack gap={0} style={{ minWidth: 0 }}>
            <Group gap={4} wrap="nowrap">
              {score?.isCurrentHolder && <Icon path={mdiCrown} size={0.6} color="var(--mantine-color-violet-6)" />}
              <Text size="sm" fw={700} truncate>
                {hill.title}
              </Text>
            </Group>
            <Text size="xs" c="dimmed" truncate>
              {isApiArena
                ? t('game.content.scoreboard.koth.api.mode', 'Leaderboard')
                : hill.currentHolderTeamName
                  ? t('game.content.scoreboard.koth.held_by', {
                      defaultValue: 'Confirmed king: {{team}}',
                      team: hill.currentHolderTeamName,
                    })
                  : hill.category}
            </Text>
          </Stack>
          <Badge size="xs" color={statusColor(status)} variant="light">
            {status ?? t('game.content.scoreboard.koth.not_checked', 'Not checked')}
          </Badge>
        </Group>

        <KothLifecycleBadges hill={hill} cycleTicks={cycleTicks} confirmationTicks={confirmationTicks} />

        {score ? (
          <>
            <SimpleGrid cols={{ base: 2, xs: 4 }} spacing="xs">
              <CompactMetric
                label={t('game.content.scoreboard.koth.epoch.column.performance', 'Hill performance')}
                value={fmtPts(score.settledPoints)}
                color="cyan"
              />
              <CompactMetric label={firstMetric} value={formatPercent(score.acquisitionRate)} color="teal" />
              <CompactMetric label={secondMetric} value={formatPercent(score.controlRate)} color="blue" />
              <CompactMetric label={thirdMetric} value={formatPercent(score.reliabilityRate)} color="orange" />
            </SimpleGrid>
            <Group justify="space-between" gap="xs" wrap="wrap">
              <Text size="xs" c="dimmed">
                {isApiArena
                  ? t('game.content.scoreboard.koth.api.evidence_value', {
                      defaultValue: '{{completed}} completed waves · {{crowns}} Crowns · {{total}} finalized waves',
                      completed: score.acquisitionWindows,
                      crowns: healthyTicks,
                      total: score.responsibleTicks,
                    })
                  : t('game.content.scoreboard.koth.evidence_value', {
                      defaultValue:
                        '{{windows}} windows · {{ticks}} controlled ticks · {{healthy}}/{{responsible}} healthy',
                      windows: score.acquisitionWindows,
                      ticks: score.controlledTicks,
                      healthy: healthyTicks,
                      responsible: score.responsibleTicks,
                    })}
              </Text>
              {hasProjection(score.settledPoints, score.projectedPoints) && (
                <Text size="xs" c={readableMetricColor('orange', dark)} className={misc.ffmono}>
                  {t('game.content.scoreboard.koth.epoch.live_value', {
                    defaultValue: 'Live {{score}}',
                    score: fmtPts(score.projectedPoints),
                  })}
                </Text>
              )}
            </Group>
          </>
        ) : (
          <Text size="sm" c="dimmed">
            {t('game.content.scoreboard.koth.no_hill_cell', 'No hill score')}
          </Text>
        )}
      </Stack>
    </Paper>
  )
}

interface KothScoreDetailModalProps {
  team: KothTeamScoreRow | null
  hills: KothScoreboardHill[]
  detailEpochLimit: number
  cycleTicks: number
  confirmationTicks: number
  onClose: () => void
}

const KothScoreDetailModal: FC<KothScoreDetailModalProps> = ({
  team,
  hills,
  detailEpochLimit,
  cycleTicks,
  confirmationTicks,
  onClose,
}) => {
  const { t } = useTranslation()
  const latestEpoch = team?.epochs.at(-1)
  const onlyApi = hills.length > 0 && hills.every((hill) => hill.claimSource === 'Api')
  const mixed = !onlyApi && hills.some((hill) => hill.claimSource === 'Api')
  const firstMetric = onlyApi
    ? t('game.content.scoreboard.koth.api.activity', 'Completion')
    : mixed
      ? t('game.content.scoreboard.koth.api.activity_acquisition', 'Completion / acquisition')
      : t('game.content.scoreboard.koth.epoch.column.acquisition', 'Acquisition')
  const secondMetric = onlyApi
    ? t('game.content.scoreboard.koth.api.objective', 'Objective')
    : mixed
      ? t('game.content.scoreboard.koth.api.objective_control', 'Objective / control')
      : t('game.content.scoreboard.koth.epoch.column.control', 'Control')
  const thirdMetric = onlyApi
    ? t('game.content.scoreboard.koth.api.integrity', 'Crown share')
    : mixed
      ? t('game.content.scoreboard.koth.api.integrity_reliability', 'Crown share / reliability')
      : t('game.content.scoreboard.koth.epoch.column.reliability', 'Reliability')

  return (
    <Modal
      key={team?.participationId ?? 'closed'}
      opened={team !== null}
      onClose={onClose}
      size="xl"
      title={
        <Group gap="sm" wrap="nowrap">
          <Avatar radius="md" color="violet">
            {team?.teamName.slice(0, 1) ?? 'T'}
          </Avatar>
          <Stack gap={0} style={{ minWidth: 0 }}>
            <Text fw={700} truncate>
              {team?.teamName}
            </Text>
            <Text size="xs" c="dimmed">
              {team?.division ?? t('game.content.scoreboard.koth.epoch.score_label', 'KotH score')}
            </Text>
          </Stack>
        </Group>
      }
    >
      {team && (
        <Stack gap="md">
          <SimpleGrid cols={{ base: 2, sm: 5 }} spacing="sm">
            <CompactMetric
              label={t('game.content.scoreboard.koth.epoch.column.event_score', 'Event score')}
              value={fmtPts(team.settledTotal)}
              color="cyan"
            />
            <CompactMetric
              label={t('game.content.scoreboard.koth.epoch.column.live_event_score', 'Live event score')}
              value={fmtPts(team.projectedTotal)}
              color="orange"
            />
            <CompactMetric label={firstMetric} value={formatPercent(team.acquisitionRate)} color="teal" />
            <CompactMetric label={secondMetric} value={formatPercent(team.controlRate)} color="blue" />
            <CompactMetric label={thirdMetric} value={formatPercent(team.reliabilityRate)} color="orange" />
          </SimpleGrid>

          <Alert color="cyan" variant="light" icon={<Icon path={mdiInformationOutline} size={0.85} />}>
            <Stack gap={4}>
              <Text size="sm" fw={700}>
                {t('game.content.scoreboard.koth.epoch.detail.event_formula_title', {
                  defaultValue: 'How the event score {{score}} is calculated',
                  score: fmtPts(team.settledTotal),
                })}
              </Text>
              {hasEpochBasis(team.settledEpochPoints, team.settledEpochWeight) ? (
                <Text size="sm" className={cx(misc.ffmono, classes.scoringFormula)}>
                  {t('game.content.scoreboard.koth.epoch.detail.event_formula', {
                    defaultValue:
                      '{{points}} weighted epoch-points ÷ {{weight}} finalized epoch weight = {{score}} event score',
                    points: formatFormulaNumber(team.settledEpochPoints),
                    weight: formatFormulaNumber(team.settledEpochWeight),
                    score: fmtPts(team.settledTotal),
                  })}
                </Text>
              ) : (
                <Text size="sm">
                  {t(
                    'game.content.scoreboard.koth.epoch.detail.no_finalized_basis',
                    'No finalized epoch evidence is available yet.'
                  )}
                </Text>
              )}
              <Text size="xs">
                {t(
                  'game.content.scoreboard.koth.epoch.detail.scope_explanation',
                  'Hill performance is a local average over that hill’s scorable evidence. It is not added directly to the event score: hills first form each epoch score, then the event score averages every finalized epoch.'
                )}
              </Text>
              {hasProjection(team.settledTotal, team.projectedTotal) &&
                hasEpochBasis(team.projectedEpochPoints, team.projectedEpochWeight) && (
                  <Text size="xs" c="orange" className={cx(misc.ffmono, classes.scoringFormula)}>
                    {t('game.content.scoreboard.koth.epoch.detail.live_event_formula', {
                      defaultValue: 'Live: {{points}} ÷ {{weight}} = {{score}}',
                      points: formatFormulaNumber(team.projectedEpochPoints),
                      weight: formatFormulaNumber(team.projectedEpochWeight),
                      score: fmtPts(team.projectedTotal),
                    })}
                  </Text>
                )}
            </Stack>
          </Alert>

          {latestEpoch && !latestEpoch.finalized && (
            <Alert color="orange" variant="light" icon={<Icon path={mdiAlertCircleOutline} size={0.85} />}>
              <Text size="sm">
                {t('game.content.scoreboard.koth.epoch.detail.current_warning', {
                  defaultValue: 'Epoch {{epoch}} is still live. Orange values are projections, not ranking points.',
                  epoch: latestEpoch.epoch,
                })}
              </Text>
            </Alert>
          )}

          <Stack gap="xs">
            <Text size="sm" fw={700}>
              {t('game.content.scoreboard.koth.epoch.hill_breakdown', 'Per-hill performance')}
            </Text>
            <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="sm">
              {hills.map((hill) => (
                <HillCard
                  key={hill.challengeId}
                  hill={hill}
                  score={team.hills.find((score) => score.challengeId === hill.challengeId)}
                  cycleTicks={cycleTicks}
                  confirmationTicks={confirmationTicks}
                />
              ))}
            </SimpleGrid>
          </Stack>

          {team.epochs.length > 0 && (
            <Stack gap={4}>
              <Text size="xs" c="dimmed">
                {t('game.content.scoreboard.koth.epoch.detail.recent_only', {
                  defaultValue: 'Latest {{limit}} epochs; totals include every epoch.',
                  limit: detailEpochLimit,
                })}
              </Text>
              <Group gap={4}>
                {[...team.epochs].reverse().map((epoch) => (
                  <Badge
                    key={epoch.epoch}
                    size="sm"
                    variant="light"
                    color={epoch.finalized ? 'gray' : 'orange'}
                    className={misc.ffmono}
                  >
                    E{epoch.epoch} {fmtPts(epoch.points)}
                  </Badge>
                ))}
              </Group>
            </Stack>
          )}
        </Stack>
      )}
    </Modal>
  )
}

interface ScoringInfoModalProps {
  opened: boolean
  onClose: () => void
  epochTicks: number
  tickSeconds: number
  currentEpoch: number
  startRound: number | null
  cycleTicks: number
  championCooldownTicks: number
  claimConfirmationTicks: number
  hasApiArena: boolean
  hasMarkerHill: boolean
}

const ScoringInfoModal: FC<ScoringInfoModalProps> = ({
  opened,
  onClose,
  epochTicks,
  tickSeconds,
  currentEpoch,
  startRound,
  cycleTicks,
  championCooldownTicks,
  claimConfirmationTicks,
  hasApiArena,
  hasMarkerHill,
}) => {
  const { t } = useTranslation()
  const currentEpochRange =
    startRound !== null && currentEpoch > 0
      ? {
          start: startRound + (currentEpoch - 1) * epochTicks,
          end: startRound + currentEpoch * epochTicks - 1,
        }
      : null

  return (
    <Modal
      opened={opened}
      onClose={onClose}
      centered
      size="sm"
      classNames={{ body: classes.scoringModalBody }}
      title={<Text fw={700}>{t('game.content.scoreboard.koth.score_info.title', 'How KotH scoring works')}</Text>}
    >
      <Stack gap="sm">
        <SimpleGrid cols={{ base: 1, xs: 2 }} spacing="xs">
          <Paper withBorder p="xs" radius="sm">
            <Group justify="space-between" gap={4} wrap="wrap">
              <Group gap={4} wrap="nowrap">
                <Icon path={mdiClockOutline} size={0.65} color="var(--mantine-color-cyan-6)" />
                <Text component="h3" size="xs" fw={700}>
                  {t('game.content.scoreboard.koth.score_info.tick_title', 'Tick / Round')}
                </Text>
              </Group>
              <Badge size="xs" color="cyan" variant="light" className={misc.ffmono} style={{ flexShrink: 0 }}>
                {t('game.content.scoreboard.koth.score_info.tick_badge', {
                  defaultValue: '{{seconds}}s',
                  seconds: tickSeconds,
                })}
              </Badge>
            </Group>
            <Text fz={11} lh={1.4} c="dimmed" mt={4}>
              {t(
                'game.content.scoreboard.koth.score_info.tick_body',
                'A tick and a round are the same live cycle. The checker observes the control token and service health once per hill.'
              )}
            </Text>
          </Paper>

          <Paper withBorder p="xs" radius="sm">
            <Group justify="space-between" gap={4} wrap="wrap">
              <Group gap={4} wrap="nowrap">
                <Icon path={mdiCounter} size={0.65} color="var(--mantine-color-yellow-6)" />
                <Text component="h3" size="xs" fw={700}>
                  {t('game.content.scoreboard.koth.score_info.epoch_title', 'Epoch')}
                </Text>
              </Group>
              <Badge size="xs" color="yellow" variant="light" className={misc.ffmono} style={{ flexShrink: 0 }}>
                {t('game.content.scoreboard.koth.score_info.epoch_badge', {
                  defaultValue: '{{count}} rounds',
                  count: epochTicks,
                })}
              </Badge>
            </Group>
            <Text fz={11} lh={1.4} c="dimmed" mt={4}>
              {currentEpochRange
                ? t('game.content.scoreboard.koth.score_info.epoch_current_body', {
                    defaultValue:
                      'Current Epoch {{epoch}} covers scoring rounds {{start}}-{{end}}. It joins Settled after every included round is final.',
                    epoch: currentEpoch,
                    start: currentEpochRange.start,
                    end: currentEpochRange.end,
                  })
                : t('game.content.scoreboard.koth.score_info.epoch_body', {
                    defaultValue: 'An epoch groups {{count}} scoring rounds into one bounded result.',
                    count: epochTicks,
                  })}
            </Text>
          </Paper>
        </SimpleGrid>

        <Text size="xs" c="dimmed">
          {t(
            'game.content.scoreboard.koth.score_info.epoch_weight',
            'Evidence-bearing complete epochs have equal weight. A wholly unavailable hill is excluded field-wide; only a shortened final epoch receives proportional weight.'
          )}
        </Text>

        <Paper withBorder p="xs" radius="sm">
          <Stack gap={4}>
            <Text size="xs" fw={700} className={cx(misc.ffmono, classes.scoringFormula)}>
              {t(
                'game.content.scoreboard.koth.score_info.event_formula',
                'Event score = Σ(epoch score × epoch weight) ÷ Σ(epoch weight)'
              )}
            </Text>
            <Text size="xs" c="dimmed">
              {t(
                'game.content.scoreboard.koth.score_info.scope',
                'Each hill column is a hill-local performance average. Hills combine into an epoch score first; the official event score then averages every finalized epoch, so one strong wave does not become the whole event score.'
              )}
            </Text>
          </Stack>
        </Paper>

        {hasMarkerHill && (
          <Text size="xs" c="dimmed">
            {t('game.content.scoreboard.koth.score_info.cycles', {
              cycleTicks,
              confirmationTicks: claimConfirmationTicks,
              cooldownTicks: championCooldownTicks,
              defaultValue:
                'Each Boot2Root crown cycle lasts {{cycleTicks}} ticks. A claim needs {{confirmationTicks}} consecutive healthy ticks; the previous cycle champion is excluded for {{cooldownTicks}} tick(s) after a healthy reset.',
            })}
          </Text>
        )}

        <Divider />

        {hasApiArena && (
          <Alert color="blue" variant="light">
            <Stack gap={4}>
              <Text size="sm" fw={700}>
                {t('game.content.scoreboard.koth.api.info_title', 'Leaderboard scoring')}
              </Text>
              <Text size="xs">
                {t(
                  'game.content.scoreboard.koth.api.info_body',
                  'Every finalized Leaderboard wave scores independently. RSCTF normalizes each completed team’s native result against the best result in that same wave, then applies one fixed curve. The arena submits evidence and one Crown assertion, never platform points.'
                )}
              </Text>
              <Text size="xs">
                {t(
                  'game.content.scoreboard.koth.api.lifecycle',
                  'The arena stays online across scoring rounds and epochs. RSCTF checks it every round and recreates it only after a stopped runtime or repeated functional-check failures; recovery time is field-wide void.'
                )}
              </Text>
              <Text size="xs" fw={700} className={cx(misc.ffmono, classes.scoringFormula)}>
                {t('game.content.scoreboard.koth.api.formula', 'Wave = 100 × [0.95 × (score ÷ best)¾ + 0.05 × Crown]')}
              </Text>
              <Text size="xs">
                {t(
                  'game.content.scoreboard.koth.api.anti_cheat',
                  'A fresh completed run is required in each wave; an omitted or incomplete team receives zero. The unique Crown earns the recurring five-point first-place bonus. On an exact top-score tie, every tied team receives full relative-performance credit and no team receives the Crown. Roster size never divides anyone’s points.'
                )}
              </Text>
            </Stack>
          </Alert>
        )}

        <Group gap="xs" align="flex-start" wrap="nowrap">
          <Icon path={mdiFlagVariantOutline} size={0.75} color="var(--mantine-color-teal-6)" />
          <Text size="sm">
            {t(
              'game.content.scoreboard.koth.score_info.acquisition',
              'Acquisition measures eligible token windows with a confirmed claim. A fast marker write alone earns no acquisition credit.'
            )}
          </Text>
        </Group>
        <Group gap="xs" align="flex-start" wrap="nowrap">
          <Icon path={mdiCrown} size={0.75} color="var(--mantine-color-blue-6)" />
          <Text size="sm">
            {t(
              'game.content.scoreboard.koth.score_info.control',
              'Control measures scorable checker ticks that observed the team’s exact current token.'
            )}
          </Text>
        </Group>
        <Group gap="xs" align="flex-start" wrap="nowrap">
          <Icon path={mdiTimerSandComplete} size={0.75} color="var(--mantine-color-orange-6)" />
          <Text size="sm">
            {t(
              'game.content.scoreboard.koth.score_info.reliability',
              'Reliability is healthy responsible ticks divided by responsible ticks and multiplies the whole local score.'
            )}
          </Text>
        </Group>
        <Paper withBorder p="xs" radius="sm">
          <Text size="xs" fw={700} className={cx(misc.ffmono, classes.scoringFormula)}>
            {t(
              'game.content.scoreboard.koth.score_info.formula',
              'Local score = 100 × reliability × (25% acquisition + 55% control + 20% √(acquisition × control))'
            )}
          </Text>
        </Paper>
        <Text size="xs" c="dimmed">
          {t(
            'game.content.scoreboard.koth.score_info.void_evidence',
            'Reset/readiness, platform failures, InternalError, and incomplete token issuance are void. Champion cooldown ticks are removed only from the affected team’s personal denominator.'
          )}
        </Text>
        <Text size="xs" c="dimmed">
          {t(
            'game.content.scoreboard.koth.score_info.settlement',
            'Rank uses finalized epochs only. Equal Settled scores are broken by control rate, reliability, confirmed acquisitions, then participation ID. Live includes the open epoch as a projection and never breaks an official tie.'
          )}
        </Text>
      </Stack>
    </Modal>
  )
}

interface KothScoreboardTableProps {
  numId: number
  scoreboard: KothScoreboardModel | undefined
  error: unknown
}

export const KothScoreboardTable: FC<KothScoreboardTableProps> = ({ numId, scoreboard, error }) => {
  const { t } = useTranslation()
  const theme = useMantineTheme()
  const { colorScheme } = useMantineColorScheme()
  const dark = colorScheme === 'dark'
  const isMobile = useIsMobile()
  const { game } = useGame(numId)
  const [detailParticipationId, setDetailParticipationId] = useState<number | null>(null)
  const [scoringInfoOpened, setScoringInfoOpened] = useState(false)

  const hillGroups = useMemo(() => {
    const groups: { category: string; items: KothScoreboardHill[] }[] = []
    for (const hill of scoreboard?.hills ?? []) {
      const last = groups.at(-1)
      if (last?.category === hill.category) last.items.push(hill)
      else groups.push({ category: hill.category, items: [hill] })
    }
    return groups
  }, [scoreboard?.hills])

  const {
    activePage,
    setPage,
    setDivisionName,
    keyword,
    setKeyword,
    highlightedTeam,
    divisionOptions,
    selectValue,
    hasDivisionFilter,
    allRank,
    filteredList,
    currentItems,
    findMyTeam,
  } = useAdLikeScoreboardState(scoreboard?.teams, game?.teamName ?? null)

  const selectedTeam = useMemo(
    () => scoreboard?.teams.find((team) => team.participationId === detailParticipationId) ?? null,
    [detailParticipationId, scoreboard?.teams]
  )
  const hasEvidence = useMemo(
    () =>
      scoreboard?.teams.some((team) =>
        team.hills.some((hill) => hill.acquisitionWindows > 0 || hill.controlledTicks > 0 || hill.responsibleTicks > 0)
      ) ?? false,
    [scoreboard?.teams]
  )
  const transitioningHills = useMemo(
    () => scoreboard?.hills.filter((hill) => hill.cycleNumber > 0 && isKothResetTransition(hill.resetPhase)) ?? [],
    [scoreboard?.hills]
  )
  const currentEpochProgress = scoreboard
    ? epochProgress(scoreboard.latestRound, scoreboard.startRound, scoreboard.epochTicks)
    : null

  const divisionRanks = useMemo(() => {
    const ranks = new Map<number, number>()
    filteredList.forEach((team, index) => {
      ranks.set(team.participationId, index + 1)
    })
    return ranks
  }, [filteredList])

  if (error) {
    return (
      <Alert color="red" icon={<Icon path={mdiAlertCircleOutline} size={0.9} />}>
        {t('game.content.scoreboard.koth.error', 'The King of the Hill scoreboard could not be loaded.')}
      </Alert>
    )
  }

  if (!scoreboard) {
    return (
      <Center py="xl">
        <Loader color="violet" />
      </Center>
    )
  }

  if (scoreboard.hills.length === 0) {
    return (
      <Paper shadow="md" p="xl">
        <Stack align="center" gap="xs">
          <Icon path={mdiCrown} size={2.5} color="var(--mantine-color-dimmed)" />
          <Text fw="bold" c="dimmed">
            {t('game.content.scoreboard.koth.empty.title', 'No hills configured')}
          </Text>
          <Text size="sm" c="dimmed">
            {t(
              'game.content.scoreboard.koth.empty.description',
              'An organizer needs to enable at least one KotH challenge.'
            )}
          </Text>
        </Stack>
      </Paper>
    )
  }

  const openDetail = (team: KothTeamScoreRow) => setDetailParticipationId(team.participationId)
  const hasApiArena = scoreboard.hills.some((hill) => hill.claimSource === 'Api')
  const onlyApiArena = scoreboard.hills.every((hill) => hill.claimSource === 'Api')
  const firstMetricLabel = onlyApiArena
    ? t('game.content.scoreboard.koth.api.activity', 'Completion')
    : hasApiArena
      ? t('game.content.scoreboard.koth.api.activity_acquisition', 'Completion / acquisition')
      : t('game.content.scoreboard.koth.epoch.column.acquisition', 'Acquisition')
  const secondMetricLabel = onlyApiArena
    ? t('game.content.scoreboard.koth.api.objective', 'Objective')
    : hasApiArena
      ? t('game.content.scoreboard.koth.api.objective_control', 'Objective / control')
      : t('game.content.scoreboard.koth.epoch.column.control', 'Control')
  const thirdMetricLabel = onlyApiArena
    ? t('game.content.scoreboard.koth.api.integrity', 'Crown share')
    : hasApiArena
      ? t('game.content.scoreboard.koth.api.integrity_reliability', 'Crown share / reliability')
      : t('game.content.scoreboard.koth.epoch.column.reliability', 'Reliability')

  return (
    <Paper shadow="md" p={{ base: 'xs', sm: 'md' }}>
      <Stack gap="xs">
        <Group justify="space-between" gap="xs" wrap="wrap">
          <Group gap="xs">
            <Icon path={mdiCrown} size={0.85} color={theme.colors.violet[6]} />
            <Stack gap={0}>
              <Group gap={4} wrap="nowrap">
                <Text size="sm" fw={800}>
                  {t('game.content.scoreboard.koth.epoch.title', 'King of the Hill scoreboard')}
                </Text>
                <Tooltip
                  label={t('game.content.scoreboard.koth.score_info.button', 'How KotH scoring works')}
                  withArrow
                >
                  <ActionIcon
                    type="button"
                    size={44}
                    variant="subtle"
                    color="violet"
                    radius="xl"
                    aria-haspopup="dialog"
                    aria-expanded={scoringInfoOpened}
                    aria-label={t('game.content.scoreboard.koth.score_info.button', 'How KotH scoring works')}
                    onClick={() => setScoringInfoOpened(true)}
                  >
                    <Icon path={mdiInformationOutline} size={0.7} />
                  </ActionIcon>
                </Tooltip>
              </Group>
              <Text size="xs" c="dimmed">
                {hasApiArena
                  ? t(
                      'game.content.scoreboard.koth.api.compact_description',
                      'Event score averages every finalized epoch and determines rank. Hill columns show local performance, not points added directly to the event score.'
                    )
                  : t(
                      'game.content.scoreboard.koth.epoch.compact_description',
                      'Event score averages every finalized epoch and determines rank. Hill columns show local performance, not points added directly to the event score.'
                    )}
              </Text>
            </Stack>
          </Group>
          <Group gap={6} wrap="wrap" justify="flex-end" ml="auto">
            <Badge color={scoreboard.fullySettled ? 'gray' : 'orange'} variant="light">
              {scoreboard.started
                ? t('game.content.scoreboard.koth.epoch.current_epoch', {
                    defaultValue: 'Epoch {{epoch}}',
                    epoch: scoreboard.currentEpoch,
                  })
                : t('game.content.scoreboard.koth.epoch.awaiting_start', 'Warmup')}
            </Badge>
            {currentEpochProgress && (
              <Badge color="blue" variant="light">
                {t('game.content.ad.epoch_tick', {
                  tick: currentEpochProgress.tick,
                  total: currentEpochProgress.totalTicks,
                  defaultValue: 'Tick {{tick}}/{{total}}',
                })}
              </Badge>
            )}
          </Group>
        </Group>

        <AdLikeToolbar
          divisionOptions={divisionOptions}
          selectValue={selectValue}
          hasDivisionFilter={hasDivisionFilter}
          onDivisionChange={setDivisionName}
          myTeamName={game?.teamName ?? null}
          onFindMyTeam={findMyTeam}
          keyword={keyword}
          onKeywordChange={setKeyword}
          currentRound={scoreboard.latestRound}
          roundEndsAt={scoreboard.currentRoundEndsAt}
          tickSeconds={scoreboard.tickSeconds}
          frozen={scoreboard.isFrozenView}
        />

        {!scoreboard.started && (
          <Alert color="yellow" variant="light" icon={<Icon path={mdiAlertCircleOutline} size={0.85} />}>
            <Text size="sm">
              {t(
                'game.content.scoreboard.koth.epoch.not_started',
                'Scoring is waiting for active, healthy KotH runtimes and ready hills.'
              )}
            </Text>
          </Alert>
        )}

        {scoreboard.started && !hasEvidence && (
          <Alert color="yellow" variant="light" icon={<Icon path={mdiAlertCircleOutline} size={0.85} />}>
            <Text size="sm">
              {t(
                'game.content.scoreboard.koth.epoch.evidence.sparse',
                'No scorable control evidence is available yet. Live values remain provisional.'
              )}
            </Text>
          </Alert>
        )}

        {transitioningHills.length > 0 && (
          <Alert color="orange" variant="light" icon={<Icon path={mdiClockOutline} size={0.85} />}>
            <Text size="sm">
              {t('game.content.scoreboard.koth.reset_pause', {
                hills: transitioningHills.map((hill) => hill.title).join(', '),
                defaultValue:
                  '{{hills}} is recovering or proving readiness. This time is excluded from every scoring denominator.',
              })}
            </Text>
          </Alert>
        )}

        {!isMobile && (
          <Box pos="relative" mih="6rem">
            <Table.ScrollContainer
              minWidth="100%"
              tabIndex={0}
              aria-label={t(
                'game.content.scoreboard.koth.scroll_region',
                'Scrollable King of the Hill scoreboard details'
              )}
            >
              <Table
                className={classes.table}
                verticalSpacing={4}
                horizontalSpacing={8}
                aria-label={t('game.content.scoreboard.koth.epoch.title', 'King of the Hill scoreboard')}
              >
                <Table.Thead className={classes.thead}>
                  <AdLikeCategoryHeaderRow groups={hillGroups} subColsPerItem={4} />
                  <Table.Tr>
                    <AdLikeHiddenCols />
                    {scoreboard.hills.map((hill) => {
                      const status =
                        hill.lastCheckStatus ?? t('game.content.scoreboard.koth.not_checked', 'Not checked')
                      return (
                        <Table.Th key={hill.challengeId} colSpan={4} className={cx(classes.mono, classes.groupStart)}>
                          <Stack gap={3} align="center">
                            <Tooltip label={hill.title} withinPortal>
                              <Text size="xs" fw={700} truncate maw={GROUP_W} mx="auto">
                                {hill.title}
                              </Text>
                            </Tooltip>
                            <Group gap={4} wrap="nowrap" maw={GROUP_W}>
                              <Badge size="xs" variant="light" color={statusColor(hill.lastCheckStatus)}>
                                {status}
                              </Badge>
                              {hill.claimSource === 'Api' && (
                                <Badge size="xs" variant="light" color="blue">
                                  {t('game.content.scoreboard.koth.api.badge', 'Leaderboard')}
                                </Badge>
                              )}
                              {hill.currentHolderTeamName && (
                                <Tooltip
                                  label={t('game.content.scoreboard.koth.held_by', {
                                    defaultValue: 'Confirmed king: {{team}}',
                                    team: hill.currentHolderTeamName,
                                  })}
                                  withinPortal
                                >
                                  <Group gap={3} wrap="nowrap" style={{ minWidth: 0 }}>
                                    <Icon path={mdiCrown} size={0.5} color={theme.colors.violet[6]} />
                                    <Text size="xs" c={readableMetricColor('violet', dark)} truncate>
                                      {hill.currentHolderTeamName}
                                    </Text>
                                  </Group>
                                </Tooltip>
                              )}
                            </Group>
                            <KothLifecycleBadges
                              hill={hill}
                              cycleTicks={scoreboard.cycleTicks}
                              confirmationTicks={scoreboard.claimConfirmationTicks}
                            />
                          </Stack>
                        </Table.Th>
                      )
                    })}
                  </Table.Tr>
                  <Table.Tr>
                    <AdLikePinnedHeaderCells
                      countLabel={
                        onlyApiArena
                          ? t('game.content.scoreboard.koth.api.active_ticks', 'Completed waves')
                          : hasApiArena
                            ? t('game.content.scoreboard.koth.api.evidence', 'Evidence')
                            : t('game.content.scoreboard.koth.epoch.column.windows', 'Windows')
                      }
                      totalLabel={t('game.content.scoreboard.koth.epoch.column.event_average', 'Event avg')}
                    />
                    {scoreboard.hills.flatMap((hill) => {
                      const isApiArena = hill.claimSource === 'Api'
                      const firstMetric = isApiArena
                        ? t('game.content.scoreboard.koth.api.activity', 'Completion')
                        : t('game.content.scoreboard.koth.epoch.column.acquisition', 'Acquisition')
                      const secondMetric = isApiArena
                        ? t('game.content.scoreboard.koth.api.objective', 'Objective')
                        : t('game.content.scoreboard.koth.epoch.column.control', 'Control')
                      const thirdMetric = isApiArena
                        ? t('game.content.scoreboard.koth.api.integrity', 'Crown share')
                        : t('game.content.scoreboard.koth.epoch.column.reliability', 'Reliability')
                      return [
                        <Table.Th
                          key={`${hill.challengeId}-score`}
                          className={cx(classes.mono, classes.groupStart)}
                          style={{ width: SUBCOL.score }}
                          aria-label={t('game.content.scoreboard.koth.epoch.column.performance', 'Hill performance')}
                        >
                          <Tooltip
                            label={t(
                              'game.content.scoreboard.koth.epoch.detail.settled_hint',
                              'Hill-local performance over scorable evidence; this is not the event score. Orange text is the live projection.'
                            )}
                            withinPortal
                          >
                            <Center>
                              <Icon path={mdiTrophyOutline} size={0.6} color={theme.colors.cyan[6]} />
                            </Center>
                          </Tooltip>
                        </Table.Th>,
                        <Table.Th
                          key={`${hill.challengeId}-acquisition`}
                          className={classes.mono}
                          style={{ width: SUBCOL.acquisition }}
                          aria-label={firstMetric}
                        >
                          <Tooltip label={firstMetric} withinPortal>
                            <Center>
                              <Icon path={mdiFlagVariantOutline} size={0.6} color={theme.colors.teal[6]} />
                            </Center>
                          </Tooltip>
                        </Table.Th>,
                        <Table.Th
                          key={`${hill.challengeId}-control`}
                          className={classes.mono}
                          style={{ width: SUBCOL.control }}
                          aria-label={secondMetric}
                        >
                          <Tooltip label={secondMetric} withinPortal>
                            <Center>
                              <Icon path={mdiCrown} size={0.6} color={theme.colors.blue[6]} />
                            </Center>
                          </Tooltip>
                        </Table.Th>,
                        <Table.Th
                          key={`${hill.challengeId}-sla`}
                          className={classes.mono}
                          style={{ width: SUBCOL.sla }}
                          aria-label={thirdMetric}
                        >
                          <Tooltip label={thirdMetric} withinPortal>
                            <Center>
                              <Icon path={mdiTimerSandComplete} size={0.6} color={theme.colors.orange[6]} />
                            </Center>
                          </Tooltip>
                        </Table.Th>,
                      ]
                    })}
                  </Table.Tr>
                </Table.Thead>
                <Table.Tbody>
                  {currentItems.map((team) => (
                    <Table.Tr
                      key={team.participationId}
                      data-team-name={team.teamName}
                      style={adLikeRowHighlight(highlightedTeam === team.teamName, theme)}
                    >
                      <AdLikePinnedRowCells
                        rank={team.rank}
                        teamName={team.teamName}
                        division={team.division}
                        total={team.settledTotal}
                        allRank={allRank}
                        tableRank={divisionRanks.get(team.participationId) ?? team.rank}
                        countValue={acquisitionWindowCount(team)}
                        totalTooltip={
                          hasEpochBasis(team.settledEpochPoints, team.settledEpochWeight)
                            ? t('game.content.scoreboard.koth.epoch.detail.event_formula', {
                                defaultValue:
                                  '{{points}} weighted epoch-points ÷ {{weight}} finalized epoch weight = {{score}} event score',
                                points: formatFormulaNumber(team.settledEpochPoints),
                                weight: formatFormulaNumber(team.settledEpochWeight),
                                score: fmtPts(team.settledTotal),
                              })
                            : t(
                                'game.content.scoreboard.koth.epoch.detail.no_finalized_basis',
                                'No finalized epoch evidence is available yet.'
                              )
                        }
                        onOpenDetail={() => openDetail(team)}
                      />

                      {scoreboard.hills.flatMap((hill) => {
                        const score = team.hills.find((item) => item.challengeId === hill.challengeId)
                        if (!score) {
                          return [
                            <Table.Td
                              key={`${hill.challengeId}-empty`}
                              colSpan={4}
                              className={cx(classes.mono, classes.groupStart)}
                            >
                              <Text size="xs" c="dimmed">
                                {t('game.content.scoreboard.koth.no_hill_cell', 'No hill score')}
                              </Text>
                            </Table.Td>,
                          ]
                        }

                        const cellBg = statusBg(hill.lastCheckStatus, theme, dark)
                        const status =
                          hill.lastCheckStatus ?? t('game.content.scoreboard.koth.not_checked', 'Not checked')
                        const isApiArena = hill.claimSource === 'Api'
                        const firstMetric = isApiArena ? 'completion' : 'acquisition'
                        const secondMetric = isApiArena ? 'objective performance' : 'control'
                        const thirdMetric = isApiArena ? 'Crown share' : 'reliability'
                        const secondEvidence = isApiArena
                          ? `${score.acquisitionWindows} completed of ${score.responsibleTicks} finalized waves`
                          : `${score.controlledTicks} positive ticks`
                        const thirdEvidence = isApiArena
                          ? `${score.healthyResponsibleTicks} Crowns of ${score.responsibleTicks} finalized waves`
                          : `${score.responsibleTicks} evidence samples`
                        const holderAccent = score.isCurrentHolder
                          ? `inset 3px 0 0 ${theme.colors.violet[dark ? 4 : 7]}`
                          : undefined
                        return [
                          <Table.Td
                            key={`${hill.challengeId}-score`}
                            className={cx(classes.mono, classes.groupStart)}
                            style={{ backgroundColor: cellBg, boxShadow: holderAccent }}
                            aria-label={`${hill.title} hill performance ${fmtPts(score.settledPoints)}${hasProjection(score.settledPoints, score.projectedPoints) ? `, live projection ${fmtPts(score.projectedPoints)}` : ''}, ${status}${score.isCurrentHolder ? ', current holder' : ''}`}
                          >
                            <Stack gap={0} align="center">
                              <Group gap={3} wrap="nowrap">
                                {score.isCurrentHolder && (
                                  <Icon path={mdiCrown} size={0.42} color={theme.colors.violet[dark ? 4 : 7]} />
                                )}
                                <Text size="xs" c={readableMetricColor('cyan', dark)} className={misc.ffmono} fw={800}>
                                  {fmtPts(score.settledPoints)}
                                </Text>
                              </Group>
                              {hasProjection(score.settledPoints, score.projectedPoints) && (
                                <Text fz={9} c={readableMetricColor('orange', dark)} className={misc.ffmono}>
                                  {t('game.content.scoreboard.koth.epoch.live_value', {
                                    defaultValue: 'Live {{score}}',
                                    score: fmtPts(score.projectedPoints),
                                  })}
                                </Text>
                              )}
                            </Stack>
                          </Table.Td>,
                          <Table.Td
                            key={`${hill.challengeId}-acquisition`}
                            className={classes.mono}
                            style={{ backgroundColor: cellBg }}
                            aria-label={`${hill.title} ${firstMetric} ${formatPercent(score.acquisitionRate)}, ${score.acquisitionWindows} ${isApiArena ? 'completed waves' : 'acquisition windows'}, ${status}`}
                          >
                            <Text size="xs" c={readableMetricColor('teal', dark)} className={misc.ffmono} fw={700}>
                              {formatPercent(score.acquisitionRate)}
                            </Text>
                          </Table.Td>,
                          <Table.Td
                            key={`${hill.challengeId}-control`}
                            className={classes.mono}
                            style={{ backgroundColor: cellBg }}
                            aria-label={`${hill.title} ${secondMetric} ${formatPercent(score.controlRate)}, ${secondEvidence}, ${status}`}
                          >
                            <Text size="xs" c={readableMetricColor('blue', dark)} className={misc.ffmono} fw={700}>
                              {formatPercent(score.controlRate)}
                            </Text>
                          </Table.Td>,
                          <Table.Td
                            key={`${hill.challengeId}-sla`}
                            className={classes.mono}
                            style={{ backgroundColor: cellBg }}
                            aria-label={`${hill.title} ${thirdMetric} ${formatPercent(score.reliabilityRate)}, ${thirdEvidence}, ${status}`}
                          >
                            <Text size="xs" c={readableMetricColor('orange', dark)} className={misc.ffmono} fw={700}>
                              {formatPercent(score.reliabilityRate)}
                            </Text>
                          </Table.Td>,
                        ]
                      })}
                      <Table.Td aria-hidden />
                    </Table.Tr>
                  ))}
                </Table.Tbody>
              </Table>
            </Table.ScrollContainer>
            <AdLikeStatusLegend />
          </Box>
        )}

        {isMobile && (
          <Box>
            <Table
              striped
              highlightOnHover
              verticalSpacing="xs"
              aria-label={t('game.content.scoreboard.koth.epoch.mobile_table', 'King of the Hill rankings')}
            >
              <Table.Thead>
                <Table.Tr>
                  <Table.Th scope="col" w={48}>
                    #
                  </Table.Th>
                  <Table.Th scope="col">{t('game.content.scoreboard.koth.column.team', 'Team')}</Table.Th>
                  <Table.Th scope="col" ta="right" w={82}>
                    {t('game.content.scoreboard.koth.epoch.column.event_score', 'Event score')}
                  </Table.Th>
                </Table.Tr>
              </Table.Thead>
              <Table.Tbody>
                {currentItems.map((team) => (
                  <Table.Tr
                    key={team.participationId}
                    data-team-name={team.teamName}
                    style={adLikeRowHighlight(highlightedTeam === team.teamName, theme)}
                  >
                    <Table.Td fw={700} className={misc.ffmono}>
                      {allRank ? team.rank : (divisionRanks.get(team.participationId) ?? team.rank)}
                    </Table.Td>
                    <Table.Td>
                      <UnstyledButton
                        onClick={() => openDetail(team)}
                        title={t('game.label.score_table.open_team_detail', {
                          defaultValue: 'Open per-hill scores for {{team}}',
                          team: team.teamName,
                        })}
                        className={classes.teamDetailButton}
                      >
                        <Group gap="xs" wrap="nowrap">
                          <Avatar size={28} radius="xl" color="violet" aria-hidden="true">
                            {team.teamName.slice(0, 1)}
                          </Avatar>
                          <Stack gap={0} style={{ minWidth: 0 }}>
                            <Text size="sm" fw={700} truncate>
                              {team.teamName}
                            </Text>
                            {team.division && (
                              <Text size="xs" c="dimmed" truncate>
                                {team.division}
                              </Text>
                            )}
                          </Stack>
                        </Group>
                      </UnstyledButton>
                    </Table.Td>
                    <Table.Td ta="right">
                      <Stack gap={0} align="flex-end">
                        <Text size="sm" fw={800} c={readableMetricColor('cyan', dark)} className={misc.ffmono}>
                          {fmtPts(team.settledTotal)}
                        </Text>
                        {hasProjection(team.settledTotal, team.projectedTotal) && (
                          <Text fz={9} c={readableMetricColor('orange', dark)} className={misc.ffmono}>
                            {t('game.content.scoreboard.koth.epoch.live_value', {
                              defaultValue: 'Live {{score}}',
                              score: fmtPts(team.projectedTotal),
                            })}
                          </Text>
                        )}
                      </Stack>
                    </Table.Td>
                  </Table.Tr>
                ))}
              </Table.Tbody>
            </Table>
            <Text mt={6} size="xs" c="dimmed">
              {t(
                'game.content.scoreboard.koth.epoch.mobile_hint',
                'Select a team to see the event-score formula and every hill’s performance.'
              )}
            </Text>
          </Box>
        )}

        {filteredList.length === 0 && (
          <Center mih="6rem">
            <Text size="sm" c="dimmed">
              {t('game.content.scoreboard.koth.no_teams', 'No teams ranked yet.')}
            </Text>
          </Center>
        )}

        <Group justify="space-between" align="flex-start" wrap="wrap" gap="md">
          <Stack gap={3}>
            <Group gap="md" wrap="wrap">
              <Group gap={4}>
                <Icon path={mdiTrophyOutline} size={0.65} color={theme.colors.cyan[6]} />
                <Text size="xs" c={readableMetricColor('cyan', dark)}>
                  {t('game.content.scoreboard.koth.epoch.column.performance', 'Hill performance')}
                </Text>
              </Group>
              <Group gap={4}>
                <Icon path={mdiFlagVariantOutline} size={0.65} color={theme.colors.teal[6]} />
                <Text size="xs" c={readableMetricColor('teal', dark)}>
                  {firstMetricLabel}
                </Text>
              </Group>
              <Group gap={4}>
                <Icon path={mdiCrown} size={0.65} color={theme.colors.blue[6]} />
                <Text size="xs" c={readableMetricColor('blue', dark)}>
                  {secondMetricLabel}
                </Text>
              </Group>
              <Group gap={4}>
                <Icon path={mdiTimerSandComplete} size={0.65} color={theme.colors.orange[6]} />
                <Text size="xs" c={readableMetricColor('orange', dark)}>
                  {thirdMetricLabel}
                </Text>
              </Group>
            </Group>
            <Text size="xs" c="dimmed">
              {hasApiArena
                ? t('game.content.scoreboard.koth.api.footer_hint', {
                    defaultValue:
                      'Leaderboard labels are shown per hill; crown icons apply only to marker hills. Updated {{time}}.',
                    time: dayjs(scoreboard.generatedAt).format('LT'),
                  })
                : t('game.content.scoreboard.koth.epoch.footer_hint', {
                    defaultValue: 'Crown marks the confirmed king. Cell tint shows latest health. Updated {{time}}.',
                    time: dayjs(scoreboard.generatedAt).format('LT'),
                  })}
            </Text>
          </Stack>
          <ScoreboardPagination
            value={activePage}
            onChange={setPage}
            total={Math.max(1, Math.ceil(filteredList.length / ITEM_COUNT_PER_PAGE))}
            boundaries={2}
          />
        </Group>
      </Stack>

      <KothScoreDetailModal
        team={selectedTeam}
        hills={scoreboard.hills}
        detailEpochLimit={scoreboard.detailEpochLimit}
        cycleTicks={scoreboard.cycleTicks}
        confirmationTicks={scoreboard.claimConfirmationTicks}
        onClose={() => setDetailParticipationId(null)}
      />
      <ScoringInfoModal
        opened={scoringInfoOpened}
        onClose={() => setScoringInfoOpened(false)}
        epochTicks={scoreboard.epochTicks}
        tickSeconds={scoreboard.tickSeconds}
        currentEpoch={scoreboard.currentEpoch}
        startRound={scoreboard.startRound}
        cycleTicks={scoreboard.cycleTicks}
        championCooldownTicks={scoreboard.championCooldownTicks}
        claimConfirmationTicks={scoreboard.claimConfirmationTicks}
        hasApiArena={hasApiArena}
        hasMarkerHill={!onlyApiArena}
      />
    </Paper>
  )
}
