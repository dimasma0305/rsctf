import {
  ActionIcon,
  Alert,
  Avatar,
  Badge,
  Box,
  Button,
  Center,
  Grid,
  Group,
  Loader,
  Modal,
  Paper,
  Progress,
  Select,
  SimpleGrid,
  Stack,
  Table,
  Text,
  TextInput,
  Tooltip,
  useMantineTheme,
} from '@mantine/core'
import {
  mdiAlertCircleOutline,
  mdiCrosshairsGps,
  mdiCrown,
  mdiFlagOutline,
  mdiInformationOutline,
  mdiMagnify,
  mdiScaleBalance,
  mdiSwordCross,
  mdiTrophyOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { adLikeRowHighlight, fmtPts, useAdLikeScoreboardState } from '@Components/AdLikeScoreboard'
import { ScoreboardPagination } from '@Components/ScoreboardPagination'
import { ScrollingText } from '@Components/ScrollingText'
import { useIsMobile } from '@Utils/ThemeOverride'
import {
  CombinedScoreComponent,
  CombinedScoreboardItem,
  CombinedScoreboardModel,
  useCombinedScoreboard,
  useGame,
} from '@Hooks/useGame'
import classes from '@Styles/CombinedScoreboard.module.css'
import misc from '@Styles/Misc.module.css'

interface ModeDefinition {
  key: 'jeopardy' | 'attackDefense' | 'koth'
  label: string
  shortLabel: string
  color: string
  icon: string
}

const hasProjection = (row: CombinedScoreboardItem) => Math.abs(row.score - row.projectedScore) > 0.005
const percent = (weight: number) => `${(weight * 100).toFixed(weight === 0.5 ? 0 : 1)}%`

const ComponentValue: FC<{
  component: CombinedScoreComponent
  definition: ModeDefinition
}> = ({ component, definition }) => {
  const { t } = useTranslation()
  const raw =
    component.earnedPoints !== undefined && component.attainablePoints !== undefined
      ? t('game.content.scoreboard.combined.jeopardy_raw', {
          defaultValue: '{{earned}} / {{attainable}} raw points',
          earned: component.earnedPoints,
          attainable: component.attainablePoints,
        })
      : null
  return (
    <Stack gap={0} align="center">
      <Text fw={800} size="sm" c={definition.color} className={misc.ffmono}>
        {fmtPts(component.score)}
      </Text>
      {Math.abs(component.score - component.projectedScore) > 0.005 && (
        <Text fz={10} c="orange" className={misc.ffmono}>
          {t('game.content.scoreboard.combined.live_short', {
            defaultValue: 'Live {{score}}',
            score: fmtPts(component.projectedScore),
          })}
        </Text>
      )}
      {raw && (
        <Text fz={10} c="dimmed" className={classes.rawPoints}>
          {raw}
        </Text>
      )}
    </Stack>
  )
}

const CombinedInfoModal: FC<{
  opened: boolean
  onClose: () => void
  scoreboard: CombinedScoreboardModel
  modes: ModeDefinition[]
}> = ({ opened, onClose, scoreboard, modes }) => {
  const { t } = useTranslation()
  return (
    <Modal
      opened={opened}
      onClose={onClose}
      centered
      size="md"
      title={<Text fw={800}>{t('game.content.scoreboard.combined.info_title', 'How Overall scoring works')}</Text>}
    >
      <Stack gap="sm">
        <Alert color="blue" icon={<Icon path={mdiScaleBalance} size={0.85} />}>
          {t(
            'game.content.scoreboard.combined.info_summary',
            'Every active format is normalized to 0-100 and receives exactly the same constant weight. The formula never compares a team with the current leader.'
          )}
        </Alert>
        <SimpleGrid cols={{ base: 1, xs: modes.length }} spacing="xs">
          {modes.map((mode) => (
            <Paper key={mode.key} withBorder p="sm" radius="md">
              <Group gap={6} wrap="nowrap">
                <Icon path={mode.icon} size={0.7} color={`var(--mantine-color-${mode.color}-6)`} />
                <Text size="sm" fw={700} truncate>
                  {mode.label}
                </Text>
                <Badge ml="auto" color={mode.color} variant="light">
                  {percent(scoreboard.modes[mode.key].weight)}
                </Badge>
              </Group>
            </Paper>
          ))}
        </SimpleGrid>
        <Text size="sm">
          {t(
            'game.content.scoreboard.combined.info_jeopardy',
            'Jeopardy = earned points ÷ the current attainable points for that division. The ceiling includes the largest configured blood bonus, so bonus points cannot overflow the scale.'
          )}
        </Text>
        <Text size="sm">
          {t(
            'game.content.scoreboard.combined.info_epochs',
            'Attack & Defense and KotH already publish bounded 0-100 scores. Overall rank uses their settled epoch scores; orange Live values include unfinished epochs only as a projection.'
          )}
        </Text>
        <Paper withBorder p="sm" radius="md">
          <Text size="sm" fw={800} ta="center" className={misc.ffmono}>
            {t('game.content.scoreboard.combined.formula', {
              defaultValue: 'Overall = ({{modes}} normalized format scores) ÷ {{count}}',
              modes: modes.map((mode) => mode.shortLabel).join(' + '),
              count: modes.length,
            })}
          </Text>
        </Paper>
      </Stack>
    </Modal>
  )
}

const MobileTeamCard: FC<{
  row: CombinedScoreboardItem
  modes: ModeDefinition[]
  allRank: boolean
  highlighted: boolean
}> = ({ row, modes, allRank, highlighted }) => {
  const { t } = useTranslation()
  const theme = useMantineTheme()
  const rank = allRank ? row.rank : (row.divisionRank ?? row.rank)
  return (
    <Paper withBorder p="sm" radius="md" data-team-name={row.name} style={adLikeRowHighlight(highlighted, theme)}>
      <Stack gap="sm">
        <Group wrap="nowrap">
          <Badge size="lg" variant="light" color="yellow" className={misc.ffmono}>
            #{rank || '-'}
          </Badge>
          <Avatar src={row.avatar} radius="xl" color="blue">
            {row.name.slice(0, 1) || 'T'}
          </Avatar>
          <Stack gap={0} style={{ minWidth: 0 }}>
            <Text fw={800} truncate>
              {row.name}
            </Text>
            {row.division && (
              <Text size="xs" c="dimmed" truncate>
                {row.division}
              </Text>
            )}
          </Stack>
          <Stack gap={0} ml="auto" ta="right">
            <Text fw={900} size="lg" className={misc.ffmono}>
              {fmtPts(row.score)}
            </Text>
            <Text fz={10} c="dimmed">
              {t('game.content.scoreboard.combined.overall', 'Overall')}
            </Text>
          </Stack>
        </Group>
        <SimpleGrid cols={modes.length} spacing="xs">
          {modes.map((mode) => {
            const component = row.components[mode.key]
            return (
              <Stack key={mode.key} gap={3} align="center">
                <Group gap={4} wrap="nowrap">
                  <Icon path={mode.icon} size={0.55} color={`var(--mantine-color-${mode.color}-6)`} />
                  <Text fz={10} fw={700} truncate>
                    {mode.shortLabel}
                  </Text>
                </Group>
                <Text size="sm" fw={800} className={misc.ffmono}>
                  {fmtPts(component.score)}
                </Text>
                <Progress value={component.score} color={mode.color} size={4} w="100%" aria-hidden="true" />
              </Stack>
            )
          })}
        </SimpleGrid>
        {hasProjection(row) && (
          <Text size="xs" c="orange" ta="right" className={misc.ffmono}>
            {t('game.content.scoreboard.combined.live_overall', {
              defaultValue: 'Live overall: {{score}}',
              score: fmtPts(row.projectedScore),
            })}
          </Text>
        )}
      </Stack>
    </Paper>
  )
}

export const CombinedScoreboardTable: FC<{ numId: number }> = ({ numId }) => {
  const { t } = useTranslation()
  const theme = useMantineTheme()
  const isMobile = useIsMobile()
  const { game } = useGame(numId)
  const { combinedScoreboard: scoreboard, error } = useCombinedScoreboard(numId)
  const [infoOpened, setInfoOpened] = useState(false)

  const modes = useMemo<ModeDefinition[]>(() => {
    if (!scoreboard) return []
    return [
      scoreboard.modes.jeopardy.active
        ? {
            key: 'jeopardy' as const,
            label: t('game.content.scoreboard.tab.jeopardy', 'Jeopardy'),
            shortLabel: t('game.content.scoreboard.tab.jeopardy', 'Jeopardy'),
            color: 'blue',
            icon: mdiFlagOutline,
          }
        : null,
      scoreboard.modes.attackDefense.active
        ? {
            key: 'attackDefense' as const,
            label: t('game.content.scoreboard.tab.ad', 'Attack & Defense'),
            shortLabel: t('game.content.scoreboard.tab.ad_short', 'A&D'),
            color: 'red',
            icon: mdiSwordCross,
          }
        : null,
      scoreboard.modes.koth.active
        ? {
            key: 'koth' as const,
            label: t('game.content.scoreboard.tab.koth', 'King of the Hill'),
            shortLabel: t('game.content.scoreboard.tab.koth_short', 'KotH'),
            color: 'violet',
            icon: mdiCrown,
          }
        : null,
    ].filter((mode): mode is ModeDefinition => mode !== null)
  }, [scoreboard, t])

  // The shared A&D-like filtering hook uses `teamName`; keep the public wire
  // model aligned with the Jeopardy board (`name`) and adapt it only in memory.
  const rows = useMemo(() => scoreboard?.items.map((row) => ({ ...row, teamName: row.name })), [scoreboard])

  const {
    activePage,
    setPage,
    setDivisionName,
    keyword,
    setKeyword,
    divisionOptions,
    selectValue,
    hasDivisionFilter,
    allRank,
    filteredList,
    currentItems,
    highlightedTeam,
    findMyTeam,
  } = useAdLikeScoreboardState(rows, game?.teamName ?? null)

  if (error) {
    return (
      <Alert color="red" icon={<Icon path={mdiAlertCircleOutline} size={0.9} />}>
        {t('game.content.scoreboard.combined.load_error', 'The Overall scoreboard could not be loaded.')}
      </Alert>
    )
  }
  if (!scoreboard) {
    return (
      <Center py="xl">
        <Loader color="yellow" />
      </Center>
    )
  }

  return (
    <Paper shadow="md" p={{ base: 'xs', sm: 'md' }}>
      <Stack gap="sm">
        <Group justify="space-between" gap="xs" wrap="wrap">
          <Group gap="xs" wrap="nowrap">
            <Icon path={mdiTrophyOutline} size={0.9} color={theme.colors.yellow[6]} />
            <Stack gap={0}>
              <Group gap={4} wrap="nowrap">
                <Text fw={800}>{t('game.content.scoreboard.combined.title', 'Overall scoreboard')}</Text>
                <Tooltip label={t('game.content.scoreboard.combined.info_button', 'How Overall scoring works')}>
                  <ActionIcon
                    type="button"
                    size={44}
                    variant="subtle"
                    color="yellow"
                    radius="xl"
                    aria-haspopup="dialog"
                    aria-expanded={infoOpened}
                    aria-label={t('game.content.scoreboard.combined.info_button', 'How Overall scoring works')}
                    onClick={() => setInfoOpened(true)}
                  >
                    <Icon path={mdiInformationOutline} size={0.7} />
                  </ActionIcon>
                </Tooltip>
              </Group>
              <Text size="xs" c="dimmed">
                {t(
                  'game.content.scoreboard.combined.description',
                  'Equal-weight, normalized 0-100 scores across every active format.'
                )}
              </Text>
            </Stack>
          </Group>
          <Group gap={5} wrap="wrap">
            {modes.map((mode) => (
              <Badge
                key={mode.key}
                color={mode.color}
                variant="light"
                leftSection={<Icon path={mode.icon} size={0.5} />}
              >
                {mode.shortLabel} · {percent(scoreboard.modes[mode.key].weight)}
              </Badge>
            ))}
            {!scoreboard.fullySettled && (
              <Badge color="orange" variant="light">
                {t('game.content.scoreboard.combined.live', 'Live projections')}
              </Badge>
            )}
          </Group>
        </Group>

        <Grid gap="xs" align="end">
          <Grid.Col span={{ base: 12, sm: 5 }}>
            <TextInput
              value={keyword}
              onChange={(event) => setKeyword(event.currentTarget.value)}
              leftSection={<Icon path={mdiMagnify} size={0.7} />}
              label={t('game.content.scoreboard.search_team', 'Search team')}
              placeholder={t('game.content.scoreboard.search_team_placeholder', 'Team name')}
            />
          </Grid.Col>
          <Grid.Col span={{ base: 12, xs: 7, sm: 4 }}>
            <Select
              value={selectValue}
              onChange={(value) => setDivisionName(value === 'all' ? null : value)}
              label={t('game.content.scoreboard.division_filter', 'Division')}
              disabled={!hasDivisionFilter}
              data={[
                { value: 'all', label: t('game.content.scoreboard.all_divisions', 'All divisions') },
                ...divisionOptions,
              ]}
            />
          </Grid.Col>
          <Grid.Col span={{ base: 12, xs: 5, sm: 3 }}>
            <Button
              fullWidth
              variant="light"
              leftSection={<Icon path={mdiCrosshairsGps} size={0.7} />}
              disabled={!game?.teamName}
              onClick={findMyTeam}
            >
              {t('game.content.scoreboard.find_my_team', 'Find my team')}
            </Button>
          </Grid.Col>
        </Grid>

        {isMobile ? (
          <Stack gap="xs">
            {currentItems.map((row) => (
              <MobileTeamCard
                key={row.id}
                row={row}
                modes={modes}
                allRank={allRank}
                highlighted={highlightedTeam === row.name}
              />
            ))}
          </Stack>
        ) : (
          <Box pos="relative">
            <Table.ScrollContainer
              minWidth={560 + modes.length * 130}
              tabIndex={0}
              aria-label={t('game.content.scoreboard.combined.scroll_region', 'Scrollable Overall scoreboard')}
            >
              <Table striped highlightOnHover verticalSpacing="sm" className={classes.table}>
                <Table.Thead>
                  <Table.Tr>
                    <Table.Th ta="center">{t('game.label.score_table.rank_total', 'Rank')}</Table.Th>
                    <Table.Th ta="center">{t('game.label.score_table.rank_division', 'Division rank')}</Table.Th>
                    <Table.Th>{t('common.label.team', 'Team')}</Table.Th>
                    <Table.Th ta="center">{t('game.content.scoreboard.combined.overall', 'Overall')}</Table.Th>
                    {modes.map((mode) => (
                      <Table.Th key={mode.key} ta="center">
                        <Tooltip label={`${mode.label} · ${percent(scoreboard.modes[mode.key].weight)}`}>
                          <Group gap={5} justify="center" wrap="nowrap">
                            <Icon path={mode.icon} size={0.6} color={`var(--mantine-color-${mode.color}-6)`} />
                            <Text size="xs" fw={700}>
                              {mode.shortLabel}
                            </Text>
                          </Group>
                        </Tooltip>
                      </Table.Th>
                    ))}
                  </Table.Tr>
                </Table.Thead>
                <Table.Tbody>
                  {currentItems.map((row) => (
                    <Table.Tr
                      key={row.id}
                      data-team-name={row.name}
                      style={adLikeRowHighlight(highlightedTeam === row.name, theme)}
                    >
                      <Table.Td ta="center" fw={800} className={misc.ffmono}>
                        {row.rank || '-'}
                      </Table.Td>
                      <Table.Td ta="center" fw={800} className={misc.ffmono}>
                        {row.divisionRank ?? '-'}
                      </Table.Td>
                      <Table.Th scope="row">
                        <Group gap="xs" wrap="nowrap" maw={240}>
                          <Avatar src={row.avatar} radius="xl" size={34} color="blue">
                            {row.name.slice(0, 1) || 'T'}
                          </Avatar>
                          <Stack gap={0} style={{ minWidth: 0, flex: 1 }}>
                            <ScrollingText size="sm" text={row.name} />
                            {row.division && (
                              <Text size="xs" c="dimmed" truncate>
                                {row.division}
                              </Text>
                            )}
                          </Stack>
                        </Group>
                      </Table.Th>
                      <Table.Td ta="center">
                        <Stack gap={0} align="center">
                          <Text fw={900} className={misc.ffmono}>
                            {fmtPts(row.score)}
                          </Text>
                          {hasProjection(row) && (
                            <Text fz={10} c="orange" className={misc.ffmono}>
                              {t('game.content.scoreboard.combined.live_short', {
                                defaultValue: 'Live {{score}}',
                                score: fmtPts(row.projectedScore),
                              })}
                            </Text>
                          )}
                        </Stack>
                      </Table.Td>
                      {modes.map((mode) => (
                        <Table.Td key={mode.key} ta="center">
                          <ComponentValue component={row.components[mode.key]} definition={mode} />
                        </Table.Td>
                      ))}
                    </Table.Tr>
                  ))}
                </Table.Tbody>
              </Table>
            </Table.ScrollContainer>
          </Box>
        )}

        {filteredList.length === 0 && (
          <Text c="dimmed" ta="center" py="xl">
            {t('game.content.scoreboard.no_teams', 'No teams match the current filters.')}
          </Text>
        )}
        <ScoreboardPagination value={activePage} onChange={setPage} total={filteredList.length} />
      </Stack>
      <CombinedInfoModal
        opened={infoOpened}
        onClose={() => setInfoOpened(false)}
        scoreboard={scoreboard}
        modes={modes}
      />
    </Paper>
  )
}
