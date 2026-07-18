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
  mdiInformationOutline,
  mdiShieldCheckOutline,
  mdiSwordCross,
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
import { useAdScoreboard, useGame } from '@Hooks/useGame'
import { AdScoreboardChallenge, AdServiceScoreModel, AdTeamScoreModel } from '@Api'
import misc from '@Styles/Misc.module.css'
import classes from '@Styles/ScoreboardTable.module.css'

// Compact fixed widths keep each challenge visually grouped, as on the
// original A&D board. Mobile uses a separate three-column table instead.
const SUBCOL = { score: 76, offense: 58, defense: 58, sla: 54 }
const GROUP_W = SUBCOL.score + SUBCOL.offense + SUBCOL.defense + SUBCOL.sla

const formatPercent = (rate: number) => `${(Math.max(0, Math.min(1, rate)) * 100).toFixed(1)}%`
const hasProjection = (settled: number, projected: number) => Math.abs(settled - projected) > 0.05
const readableMetricColor = (color: string, dark: boolean) => `${color}.${dark ? 4 : 9}`
const servicesFor = (team: AdTeamScoreModel) => team.services ?? []

const captureCount = (team: AdTeamScoreModel) =>
  servicesFor(team).reduce((total, service) => total + service.captureCount, 0)

interface CompactMetricProps {
  label: string
  value: string
  color?: string
}

const CompactMetric: FC<CompactMetricProps> = ({ label, value, color }) => (
  <CompactMetricValue label={label} value={value} color={color} />
)

const CompactMetricValue: FC<CompactMetricProps> = ({ label, value, color }) => {
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

interface ServiceCardProps {
  challenge: AdScoreboardChallenge
  service?: AdServiceScoreModel
}

const ServiceCard: FC<ServiceCardProps> = ({ challenge, service }) => {
  const { t } = useTranslation()
  const { colorScheme } = useMantineColorScheme()
  const status = service?.lastCheckStatus

  return (
    <Paper withBorder p="sm" radius="md">
      <Stack gap="sm">
        <Group justify="space-between" gap="xs" wrap="nowrap">
          <Stack gap={0} style={{ minWidth: 0 }}>
            <Text size="sm" fw={700} truncate>
              {challenge.title}
            </Text>
            <Text size="xs" c="dimmed">
              {challenge.category}
            </Text>
          </Stack>
          <Badge size="xs" color={statusColor(status)} variant="light">
            {status ?? t('game.content.scoreboard.ad.not_checked', 'Not checked')}
          </Badge>
        </Group>

        {service ? (
          <>
            <SimpleGrid cols={{ base: 2, xs: 4 }} spacing="xs">
              <CompactMetric
                label={t('game.content.scoreboard.ad.epoch.column.contribution', 'Contribution')}
                value={fmtPts(service.settledPoints)}
                color="cyan"
              />
              <CompactMetric
                label={t('game.content.scoreboard.ad.epoch.column.offense', 'Offense')}
                value={formatPercent(service.offenseRate)}
                color="teal"
              />
              <CompactMetric
                label={t('game.content.scoreboard.ad.epoch.column.defense', 'Defense')}
                value={formatPercent(service.defenseRate)}
                color="blue"
              />
              <CompactMetric
                label={t('game.content.scoreboard.ad.epoch.column.sla', 'SLA')}
                value={formatPercent(service.slaRate)}
                color="orange"
              />
            </SimpleGrid>
            <Group justify="space-between" gap="xs">
              <Text size="xs" c="dimmed">
                {t('game.content.scoreboard.ad.epoch.captures_value', {
                  defaultValue: '{{count}} captures',
                  count: service.captureCount,
                })}
              </Text>
              {hasProjection(service.settledPoints, service.projectedPoints) && (
                <Text size="xs" c={readableMetricColor('orange', colorScheme === 'dark')} className={misc.ffmono}>
                  {t('game.content.scoreboard.ad.epoch.live_value', {
                    defaultValue: 'Live {{score}}',
                    score: fmtPts(service.projectedPoints),
                  })}
                </Text>
              )}
            </Group>
          </>
        ) : (
          <Text size="sm" c="dimmed">
            {t('game.content.scoreboard.ad.no_service_cell', 'No service')}
          </Text>
        )}
      </Stack>
    </Paper>
  )
}

interface AdScoreDetailModalProps {
  team: AdTeamScoreModel | null
  challenges: AdScoreboardChallenge[]
  detailEpochLimit: number
  onClose: () => void
}

const AdScoreDetailModal: FC<AdScoreDetailModalProps> = ({ team, challenges, detailEpochLimit, onClose }) => {
  const { t } = useTranslation()
  const latestEpoch = team?.epochs.at(-1)

  return (
    <Modal
      key={team?.participationId ?? 'closed'}
      opened={team !== null}
      onClose={onClose}
      size="xl"
      title={
        <Group gap="sm" wrap="nowrap">
          <Avatar radius="md" color="cyan">
            {team?.teamName.slice(0, 1) ?? 'T'}
          </Avatar>
          <Stack gap={0} style={{ minWidth: 0 }}>
            <Text fw={700} truncate>
              {team?.teamName}
            </Text>
            <Text size="xs" c="dimmed">
              {team?.division ?? t('game.content.scoreboard.ad.epoch.score_label', 'A&D score')}
            </Text>
          </Stack>
        </Group>
      }
    >
      {team && (
        <Stack gap="md">
          <SimpleGrid cols={{ base: 2, sm: 5 }} spacing="sm">
            <CompactMetric
              label={t('game.content.scoreboard.ad.epoch.column.settled', 'Settled')}
              value={fmtPts(team.settledTotal)}
              color="cyan"
            />
            <CompactMetric
              label={t('game.content.scoreboard.ad.epoch.column.projected', 'Live')}
              value={fmtPts(team.projectedTotal)}
              color="orange"
            />
            <CompactMetric
              label={t('game.content.scoreboard.ad.epoch.column.offense', 'Offense')}
              value={formatPercent(team.offenseRate)}
              color="teal"
            />
            <CompactMetric
              label={t('game.content.scoreboard.ad.epoch.column.defense', 'Defense')}
              value={formatPercent(team.defenseRate)}
              color="blue"
            />
            <CompactMetric
              label={t('game.content.scoreboard.ad.epoch.column.sla', 'SLA')}
              value={formatPercent(team.slaRate)}
              color="orange"
            />
          </SimpleGrid>

          {latestEpoch && !latestEpoch.finalized && (
            <Alert color="orange" variant="light" icon={<Icon path={mdiAlertCircleOutline} size={0.85} />}>
              <Text size="sm">
                {t('game.content.scoreboard.ad.epoch.detail.current_warning', {
                  defaultValue: 'Epoch {{epoch}} is still live. Orange values are projections, not ranking points.',
                  epoch: latestEpoch.epoch,
                })}
              </Text>
            </Alert>
          )}

          <Stack gap="xs">
            <Text size="sm" fw={700}>
              {t('game.content.scoreboard.ad.epoch.service_breakdown', 'Per-challenge score')}
            </Text>
            <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="sm">
              {challenges.map((challenge) => (
                <ServiceCard
                  key={challenge.challengeId}
                  challenge={challenge}
                  service={servicesFor(team).find((service) => service.challengeId === challenge.challengeId)}
                />
              ))}
            </SimpleGrid>
          </Stack>

          {team.epochs.length > 0 && (
            <Stack gap={4}>
              <Text size="xs" c="dimmed">
                {t('game.content.scoreboard.ad.epoch.detail.recent_only', {
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
}

const ScoringInfoModal: FC<ScoringInfoModalProps> = ({
  opened,
  onClose,
  epochTicks,
  tickSeconds,
  currentEpoch,
  startRound,
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
      title={<Text fw={700}>{t('game.content.scoreboard.ad.epoch.score_info.title', 'How scoring works')}</Text>}
    >
      <Stack gap="sm">
        <SimpleGrid cols={{ base: 1, xs: 2 }} spacing="xs">
          <Paper withBorder p="xs" radius="sm">
            <Group justify="space-between" gap={4} wrap="wrap">
              <Group gap={4} wrap="nowrap">
                <Icon path={mdiClockOutline} size={0.65} color="var(--mantine-color-cyan-6)" />
                <Text component="h3" size="xs" fw={700}>
                  {t('game.content.scoreboard.ad.epoch.score_info.tick_title', 'Tick / Round')}
                </Text>
              </Group>
              <Badge size="xs" color="cyan" variant="light" className={misc.ffmono} style={{ flexShrink: 0 }}>
                {t('game.content.scoreboard.ad.epoch.score_info.tick_badge', {
                  defaultValue: '{{seconds}}s',
                  seconds: tickSeconds,
                })}
              </Badge>
            </Group>
            <Text fz={11} lh={1.4} c="dimmed" mt={4}>
              {t(
                'game.content.scoreboard.ad.epoch.score_info.tick_body',
                'A tick and a round are the same live cycle. Fresh flags are planted, checkers run, and captures plus SLA evidence are recorded.'
              )}
            </Text>
          </Paper>

          <Paper withBorder p="xs" radius="sm">
            <Group justify="space-between" gap={4} wrap="wrap">
              <Group gap={4} wrap="nowrap">
                <Icon path={mdiCounter} size={0.65} color="var(--mantine-color-yellow-6)" />
                <Text component="h3" size="xs" fw={700}>
                  {t('game.content.scoreboard.ad.epoch.score_info.epoch_title', 'Epoch')}
                </Text>
              </Group>
              <Badge size="xs" color="yellow" variant="light" className={misc.ffmono} style={{ flexShrink: 0 }}>
                {t('game.content.scoreboard.ad.epoch.score_info.epoch_badge', {
                  defaultValue: '{{count}} rounds',
                  count: epochTicks,
                })}
              </Badge>
            </Group>
            <Text fz={11} lh={1.4} c="dimmed" mt={4}>
              {currentEpochRange
                ? t('game.content.scoreboard.ad.epoch.score_info.epoch_current_body', {
                    defaultValue:
                      'Current Epoch {{epoch}} covers scoring rounds {{start}}-{{end}}. It appears in Live while open and joins Settled only after its checks finish and flag submission windows close.',
                    epoch: currentEpoch,
                    start: currentEpochRange.start,
                    end: currentEpochRange.end,
                  })
                : t('game.content.scoreboard.ad.epoch.score_info.epoch_body', {
                    defaultValue:
                      'An epoch groups {{count}} scoring rounds. It appears in Live while open and joins Settled only after its checks finish and flag submission windows close.',
                    count: epochTicks,
                  })}
            </Text>
          </Paper>
        </SimpleGrid>

        <Text size="xs" c="dimmed">
          {t(
            'game.content.scoreboard.ad.epoch.score_info.epoch_weight',
            'Old flags may remain valid in later rounds; a capture counts in the epoch where its flag was planted. Complete epochs have equal weight, while a shortened final epoch is weighted by rounds completed.'
          )}
        </Text>

        <Divider />

        <Text size="sm">
          {t(
            'game.content.scoreboard.ad.epoch.score_info.intro',
            'Challenge contributions add up to the team total and already include challenge and epoch weighting.'
          )}
        </Text>
        <Group gap="xs" align="flex-start" wrap="nowrap">
          <Icon path={mdiSwordCross} size={0.75} color="var(--mantine-color-teal-6)" />
          <Text size="sm">
            {t(
              'game.content.scoreboard.ad.epoch.score_info.offense',
              'Offense measures accepted capture coverage, with a small bounded rarity bonus for uncommon captures.'
            )}
          </Text>
        </Group>
        <Group gap="xs" align="flex-start" wrap="nowrap">
          <Icon path={mdiShieldCheckOutline} size={0.75} color="var(--mantine-color-blue-6)" />
          <Text size="sm">
            {t(
              'game.content.scoreboard.ad.epoch.score_info.defense',
              'Defense measures the share of eligible opponent-flag pairs that remain uncaptured.'
            )}
          </Text>
        </Group>
        <Group gap="xs" align="flex-start" wrap="nowrap">
          <Icon path={mdiTimerSandComplete} size={0.75} color="var(--mantine-color-orange-6)" />
          <Text size="sm">
            {t(
              'game.content.scoreboard.ad.epoch.score_info.sla',
              'SLA is checker-measured reliability and multiplies the whole local score.'
            )}
          </Text>
        </Group>
        <Paper withBorder p="xs" radius="sm">
          <Text size="xs" fw={700} className={cx(misc.ffmono, classes.scoringFormula)}>
            {t(
              'game.content.scoreboard.ad.epoch.score_info.formula',
              'Local score = 100 × SLA × (40% offense + 40% defense + 20% √(offense × defense))'
            )}
          </Text>
        </Paper>
        <Text size="xs" c="dimmed">
          {t(
            'game.content.scoreboard.ad.epoch.score_info.settlement',
            'Rank is ordered by Settled score. Exact ties use Live, offense, defense, SLA, then participation ID. Live includes the open epoch and can change until it settles.'
          )}
        </Text>
      </Stack>
    </Modal>
  )
}

interface AdScoreboardTableProps {
  numId: number
}

export const AdScoreboardTable: FC<AdScoreboardTableProps> = ({ numId }) => {
  const { t } = useTranslation()
  const theme = useMantineTheme()
  const { colorScheme } = useMantineColorScheme()
  const dark = colorScheme === 'dark'
  const isMobile = useIsMobile()
  const { game } = useGame(numId)
  const { adScoreboard: scoreboard, error } = useAdScoreboard(numId)
  const [detailParticipationId, setDetailParticipationId] = useState<number | null>(null)
  const [scoringInfoOpened, setScoringInfoOpened] = useState(false)

  const challengeGroups = useMemo(() => {
    const groups: { category: string; items: AdScoreboardChallenge[] }[] = []
    for (const challenge of scoreboard?.challenges ?? []) {
      const last = groups.at(-1)
      if (last?.category === challenge.category) last.items.push(challenge)
      else groups.push({ category: challenge.category, items: [challenge] })
    }
    return groups
  }, [scoreboard?.challenges])

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
    base,
    currentItems,
    findMyTeam,
  } = useAdLikeScoreboardState(scoreboard?.teams, game?.teamName ?? null)

  if (error) {
    return (
      <Alert color="red" icon={<Icon path={mdiAlertCircleOutline} size={0.9} />}>
        {t('game.content.scoreboard.ad.epoch.load_error', 'The A&D scoreboard could not be loaded.')}
      </Alert>
    )
  }

  if (!scoreboard) {
    return (
      <Center py="xl">
        <Loader color="cyan" />
      </Center>
    )
  }

  const selectedTeam = scoreboard.teams.find((team) => team.participationId === detailParticipationId) ?? null
  const sparseEvidence = scoreboard.evidence.eligibleFlags === 0
  const noDefenseEvidence = scoreboard.evidence.defenseOpportunities === 0
  const currentEpochProgress = epochProgress(scoreboard.latestRound, scoreboard.startRound, scoreboard.epochTicks)
  const openDetail = (team: AdTeamScoreModel) => setDetailParticipationId(team.participationId)

  return (
    <Paper shadow="md" p={{ base: 'xs', sm: 'md' }}>
      <Stack gap="xs">
        <Group justify="space-between" gap="xs" wrap="wrap">
          <Group gap="xs">
            <Icon path={mdiTrophyOutline} size={0.85} color={theme.colors.cyan[6]} />
            <Stack gap={0}>
              <Group gap={4} wrap="nowrap">
                <Text size="sm" fw={800}>
                  {t('game.content.scoreboard.ad.epoch.title', 'Attack & Defense scoreboard')}
                </Text>
                <Tooltip
                  label={t('game.content.scoreboard.ad.epoch.score_info.button', 'How A&D scoring works')}
                  withArrow
                >
                  <ActionIcon
                    type="button"
                    size={44}
                    variant="subtle"
                    color="cyan"
                    radius="xl"
                    aria-haspopup="dialog"
                    aria-expanded={scoringInfoOpened}
                    aria-label={t('game.content.scoreboard.ad.epoch.score_info.button', 'How A&D scoring works')}
                    onClick={() => setScoringInfoOpened(true)}
                  >
                    <Icon path={mdiInformationOutline} size={0.7} />
                  </ActionIcon>
                </Tooltip>
              </Group>
              <Text size="xs" c="dimmed">
                {t('game.content.scoreboard.ad.epoch.compact_description', 'Rank is ordered by Settled score.')}
              </Text>
            </Stack>
          </Group>
          <Group gap={6} wrap="wrap" justify="flex-end" ml="auto">
            <Badge color={scoreboard.fullySettled ? 'gray' : 'orange'} variant="light">
              {scoreboard.started
                ? t('game.content.scoreboard.ad.epoch.current_epoch', {
                    defaultValue: 'Epoch {{epoch}}',
                    epoch: scoreboard.currentEpoch,
                  })
                : t('game.content.scoreboard.ad.epoch.awaiting_roster', 'Warmup')}
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
                'game.content.scoreboard.ad.epoch.not_started',
                'Scoring is waiting for a complete roster and prepared exact checkers.'
              )}
            </Text>
          </Alert>
        )}

        {scoreboard.started && sparseEvidence && (
          <Alert color="yellow" variant="light" icon={<Icon path={mdiAlertCircleOutline} size={0.85} />}>
            <Text size="sm">
              {t(
                'game.content.scoreboard.ad.epoch.evidence.sparse',
                'No eligible flag evidence is available yet. Live values remain provisional.'
              )}
            </Text>
          </Alert>
        )}

        {scoreboard.started && !sparseEvidence && noDefenseEvidence && (
          <Alert color="yellow" variant="light" icon={<Icon path={mdiAlertCircleOutline} size={0.85} />}>
            <Text size="sm">
              {t(
                'game.content.scoreboard.ad.epoch.evidence.no_defense',
                'No defense opportunities are qualified yet; exact healthy checks are required.'
              )}
            </Text>
          </Alert>
        )}

        {!isMobile && (
          <Box pos="relative" mih="6rem">
            <Table.ScrollContainer
              minWidth="100%"
              tabIndex={0}
              aria-label={t(
                'game.content.scoreboard.ad.scroll_region',
                'Scrollable Attack & Defense scoreboard details'
              )}
            >
              <Table
                className={classes.table}
                verticalSpacing={4}
                horizontalSpacing={8}
                aria-label={t('game.content.scoreboard.ad.epoch.title', 'Attack & Defense scoreboard')}
              >
                <Table.Thead className={classes.thead}>
                  <AdLikeCategoryHeaderRow groups={challengeGroups} subColsPerItem={4} />
                  <Table.Tr>
                    <AdLikeHiddenCols />
                    {scoreboard.challenges.map((challenge) => (
                      <Table.Th
                        key={challenge.challengeId}
                        colSpan={4}
                        className={cx(classes.mono, classes.groupStart)}
                      >
                        <Tooltip label={challenge.title} withinPortal>
                          <Text size="xs" fw={700} truncate maw={GROUP_W} mx="auto">
                            {challenge.title}
                          </Text>
                        </Tooltip>
                      </Table.Th>
                    ))}
                  </Table.Tr>
                  <Table.Tr>
                    <AdLikePinnedHeaderCells
                      countLabel={t('game.content.scoreboard.ad.column.captures', 'Captures')}
                      totalLabel={t('game.content.scoreboard.ad.epoch.column.settled', 'Settled')}
                    />
                    {scoreboard.challenges.flatMap((challenge) => [
                      <Table.Th
                        key={`${challenge.challengeId}-score`}
                        className={cx(classes.mono, classes.groupStart)}
                        style={{ width: SUBCOL.score }}
                        aria-label={t('game.content.scoreboard.ad.epoch.column.contribution', 'Contribution')}
                      >
                        <Tooltip
                          label={t(
                            'game.content.scoreboard.ad.epoch.detail.settled_hint',
                            'Finalized challenge contribution. Challenge contributions add up to the team total; orange text is the live projection.'
                          )}
                          withinPortal
                        >
                          <Center>
                            <Icon path={mdiTrophyOutline} size={0.6} color={theme.colors.cyan[6]} />
                          </Center>
                        </Tooltip>
                      </Table.Th>,
                      <Table.Th
                        key={`${challenge.challengeId}-offense`}
                        className={classes.mono}
                        style={{ width: SUBCOL.offense }}
                        aria-label={t('game.content.scoreboard.ad.epoch.column.offense', 'Offense')}
                      >
                        <Tooltip label={t('game.content.scoreboard.ad.epoch.column.offense', 'Offense')} withinPortal>
                          <Center>
                            <Icon path={mdiSwordCross} size={0.6} color={theme.colors.teal[6]} />
                          </Center>
                        </Tooltip>
                      </Table.Th>,
                      <Table.Th
                        key={`${challenge.challengeId}-defense`}
                        className={classes.mono}
                        style={{ width: SUBCOL.defense }}
                        aria-label={t('game.content.scoreboard.ad.epoch.column.defense', 'Defense')}
                      >
                        <Tooltip label={t('game.content.scoreboard.ad.epoch.column.defense', 'Defense')} withinPortal>
                          <Center>
                            <Icon path={mdiShieldCheckOutline} size={0.6} color={theme.colors.blue[6]} />
                          </Center>
                        </Tooltip>
                      </Table.Th>,
                      <Table.Th
                        key={`${challenge.challengeId}-sla`}
                        className={classes.mono}
                        style={{ width: SUBCOL.sla }}
                        aria-label={t('game.content.scoreboard.ad.epoch.column.sla', 'SLA')}
                      >
                        <Tooltip label={t('game.content.scoreboard.ad.epoch.column.sla', 'SLA')} withinPortal>
                          <Center>
                            <Icon path={mdiTimerSandComplete} size={0.6} color={theme.colors.orange[6]} />
                          </Center>
                        </Tooltip>
                      </Table.Th>,
                    ])}
                  </Table.Tr>
                </Table.Thead>
                <Table.Tbody>
                  {currentItems.map((team, index) => (
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
                        tableRank={base + index + 1}
                        countValue={captureCount(team)}
                        onOpenDetail={() => openDetail(team)}
                      />

                      {scoreboard.challenges.flatMap((challenge) => {
                        const service = servicesFor(team).find((item) => item.challengeId === challenge.challengeId)
                        if (!service) {
                          return [
                            <Table.Td
                              key={`${challenge.challengeId}-empty`}
                              colSpan={4}
                              className={cx(classes.mono, classes.groupStart)}
                            >
                              <Text size="xs" c="dimmed">
                                {t('game.content.scoreboard.ad.no_service_cell', 'No service')}
                              </Text>
                            </Table.Td>,
                          ]
                        }

                        const cellBg = statusBg(service.lastCheckStatus, theme, dark)
                        const status =
                          service.lastCheckStatus ?? t('game.content.scoreboard.ad.not_checked', 'Not checked')
                        return [
                          <Table.Td
                            key={`${challenge.challengeId}-score`}
                            className={cx(classes.mono, classes.groupStart)}
                            style={{ backgroundColor: cellBg }}
                            aria-label={`${challenge.title} score ${fmtPts(service.settledPoints)}, ${status}`}
                          >
                            <Stack gap={0} align="center">
                              <Text size="xs" c={readableMetricColor('cyan', dark)} className={misc.ffmono} fw={800}>
                                {fmtPts(service.settledPoints)}
                              </Text>
                              {hasProjection(service.settledPoints, service.projectedPoints) && (
                                <Text fz={9} c={readableMetricColor('orange', dark)} className={misc.ffmono}>
                                  {t('game.content.scoreboard.ad.epoch.live_value', {
                                    defaultValue: 'Live {{score}}',
                                    score: fmtPts(service.projectedPoints),
                                  })}
                                </Text>
                              )}
                            </Stack>
                          </Table.Td>,
                          <Table.Td
                            key={`${challenge.challengeId}-offense`}
                            className={classes.mono}
                            style={{ backgroundColor: cellBg }}
                            aria-label={`${challenge.title} offense ${formatPercent(service.offenseRate)}, ${status}`}
                          >
                            <Text size="xs" c={readableMetricColor('teal', dark)} className={misc.ffmono} fw={700}>
                              {formatPercent(service.offenseRate)}
                            </Text>
                          </Table.Td>,
                          <Table.Td
                            key={`${challenge.challengeId}-defense`}
                            className={classes.mono}
                            style={{ backgroundColor: cellBg }}
                            aria-label={`${challenge.title} defense ${formatPercent(service.defenseRate)}, ${status}`}
                          >
                            <Text size="xs" c={readableMetricColor('blue', dark)} className={misc.ffmono} fw={700}>
                              {formatPercent(service.defenseRate)}
                            </Text>
                          </Table.Td>,
                          <Table.Td
                            key={`${challenge.challengeId}-sla`}
                            className={classes.mono}
                            style={{ backgroundColor: cellBg }}
                            aria-label={`${challenge.title} SLA ${formatPercent(service.slaRate)}, ${status}`}
                          >
                            <Text size="xs" c={readableMetricColor('orange', dark)} className={misc.ffmono} fw={700}>
                              {formatPercent(service.slaRate)}
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
              aria-label={t('game.content.scoreboard.ad.epoch.mobile_table', 'A&D rankings')}
            >
              <Table.Thead>
                <Table.Tr>
                  <Table.Th scope="col" w={48}>
                    #
                  </Table.Th>
                  <Table.Th scope="col">{t('game.content.scoreboard.ad.column.team', 'Team')}</Table.Th>
                  <Table.Th scope="col" ta="right" w={82}>
                    {t('game.content.scoreboard.ad.epoch.column.settled', 'Settled')}
                  </Table.Th>
                </Table.Tr>
              </Table.Thead>
              <Table.Tbody>
                {currentItems.map((team, index) => (
                  <Table.Tr
                    key={team.participationId}
                    data-team-name={team.teamName}
                    style={adLikeRowHighlight(highlightedTeam === team.teamName, theme)}
                  >
                    <Table.Td fw={700} className={misc.ffmono}>
                      {allRank ? team.rank : base + index + 1}
                    </Table.Td>
                    <Table.Td>
                      <UnstyledButton
                        onClick={() => openDetail(team)}
                        title={t('game.label.score_table.open_team_detail', {
                          defaultValue: 'Open per-challenge scores for {{team}}',
                          team: team.teamName,
                        })}
                        className={classes.teamDetailButton}
                      >
                        <Group gap="xs" wrap="nowrap">
                          <Avatar size={28} radius="xl" color="cyan" aria-hidden="true">
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
                            {t('game.content.scoreboard.ad.epoch.live_value', {
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
              {t('game.content.scoreboard.ad.epoch.mobile_hint', 'Select a team to see every challenge score.')}
            </Text>
          </Box>
        )}

        {filteredList.length === 0 && (
          <Center mih="6rem">
            <Text size="sm" c="dimmed">
              {t('game.content.scoreboard.ad.no_teams', 'No teams ranked yet.')}
            </Text>
          </Center>
        )}

        <Group justify="space-between" align="flex-start" wrap="wrap" gap="md">
          <Stack gap={3}>
            <Group gap="md" wrap="wrap">
              <Group gap={4}>
                <Icon path={mdiTrophyOutline} size={0.65} color={theme.colors.cyan[6]} />
                <Text size="xs" c={readableMetricColor('cyan', dark)}>
                  {t('game.content.scoreboard.ad.epoch.column.settled', 'Settled score')}
                </Text>
              </Group>
              <Group gap={4}>
                <Icon path={mdiSwordCross} size={0.65} color={theme.colors.teal[6]} />
                <Text size="xs" c={readableMetricColor('teal', dark)}>
                  {t('game.content.scoreboard.ad.epoch.column.offense', 'Offense')}
                </Text>
              </Group>
              <Group gap={4}>
                <Icon path={mdiShieldCheckOutline} size={0.65} color={theme.colors.blue[6]} />
                <Text size="xs" c={readableMetricColor('blue', dark)}>
                  {t('game.content.scoreboard.ad.epoch.column.defense', 'Defense')}
                </Text>
              </Group>
              <Group gap={4}>
                <Icon path={mdiTimerSandComplete} size={0.65} color={theme.colors.orange[6]} />
                <Text size="xs" c={readableMetricColor('orange', dark)}>
                  {t('game.content.scoreboard.ad.epoch.column.sla', 'SLA')}
                </Text>
              </Group>
            </Group>
            <Text size="xs" c="dimmed">
              {t('game.content.scoreboard.ad.epoch.footer_hint', {
                defaultValue: 'Defense is positive. Cell tint shows latest health. Updated {{time}}.',
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

      <AdScoreDetailModal
        team={selectedTeam}
        challenges={scoreboard.challenges}
        detailEpochLimit={scoreboard.detailEpochLimit}
        onClose={() => setDetailParticipationId(null)}
      />
      <ScoringInfoModal
        opened={scoringInfoOpened}
        onClose={() => setScoringInfoOpened(false)}
        epochTicks={scoreboard.epochTicks}
        tickSeconds={scoreboard.tickSeconds}
        currentEpoch={scoreboard.currentEpoch}
        startRound={scoreboard.startRound}
      />
    </Paper>
  )
}
