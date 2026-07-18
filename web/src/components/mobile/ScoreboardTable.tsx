import {
  Avatar,
  Badge,
  Box,
  Button,
  Group,
  Paper,
  Select,
  Stack,
  Table,
  Text,
  TextInput,
  UnstyledButton,
  alpha,
  useMantineTheme,
} from '@mantine/core'
import { useDebouncedValue } from '@mantine/hooks'
import { mdiCrosshairsGps, mdiMagnify } from '@mdi/js'
import { Icon } from '@mdi/react'
import cx from 'clsx'
import React, { FC, useEffect, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { ScoreboardPagination } from '@Components/ScoreboardPagination'
import { ScoreboardProps } from '@Components/ScoreboardTable'
import { ScrollingText } from '@Components/ScrollingText'
import { MobileScoreboardItemModal } from '@Components/mobile/ScoreboardItemModal'
import { BloodBonus, BloodsTypes, useBonusLabels } from '@Utils/Shared'
import { useGame, useGameScoreboard } from '@Hooks/useGame'
import { ScoreboardItem } from '@Api'
import classes from '@Styles/ScoreboardTable.module.css'

const RANK_WIDTH = 48
const SOLVED_WIDTH = 58
const SCORE_WIDTH = 68
const ITEM_COUNT_PER_PAGE = 10

const TableRow: FC<{
  item: ScoreboardItem
  displayRank: number
  highlighted?: boolean
  onOpenDetail: () => void
}> = React.memo(({ item, displayRank, highlighted, onOpenDetail }) => {
  const theme = useMantineTheme()
  const { t } = useTranslation()
  const solved = item.solvedChallenges
  const totalScore = useMemo(() => solved?.reduce((acc, cur) => acc + (cur?.score ?? 0), 0) ?? 0, [solved])

  return (
    <Table.Tr
      data-team-name={item.name ?? ''}
      style={
        highlighted
          ? {
              outline: `2px solid ${theme.colors[theme.primaryColor][4]}`,
              outlineOffset: -2,
              background: alpha(theme.colors[theme.primaryColor][4], 0.12),
            }
          : undefined
      }
    >
      <Table.Td className={cx(classes.mono, classes.left)}>{displayRank || '-'}</Table.Td>
      <Table.Th scope="row" className={cx(classes.left, classes.teamCell)}>
        <UnstyledButton
          onClick={onOpenDetail}
          title={t('game.label.score_table.open_team_detail', {
            defaultValue: 'Open details for {{team}}',
            team: item.name || t('common.label.team', 'team'),
          })}
          className={classes.teamDetailButton}
        >
          <Group justify="left" gap={7} wrap="nowrap" style={{ minWidth: 0, flex: 1 }}>
            <Avatar alt="" aria-hidden="true" src={item.avatar} radius="xl" size={32} color={theme.primaryColor}>
              {item.name?.slice(0, 1) ?? 'T'}
            </Avatar>
            <ScrollingText text={item.name || ''} size="sm" fw={650} style={{ width: '100%', minWidth: 0 }} />
          </Group>
        </UnstyledButton>
      </Table.Th>
      <Table.Td className={cx(classes.mono, classes.left)}>{solved?.length ?? 0}</Table.Td>
      <Table.Td className={cx(classes.mono, classes.left)}>{totalScore}</Table.Td>
    </Table.Tr>
  )
})

export const MobileScoreboardTable: FC<ScoreboardProps> = ({ divisionId, setDivisionId }) => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')
  const [activePage, setPage] = useState(1)
  const [bloodBonus, setBloodBonus] = useState(BloodBonus.default)
  const [keyword, setKeyword] = useState('')
  const [debouncedKeyword] = useDebouncedValue(keyword, 250)
  const [highlightedTeam, setHighlightedTeam] = useState<string | null>(null)
  const { scoreboard } = useGameScoreboard(numId)
  const { game } = useGame(numId)
  const myTeamName = game?.teamName ?? null
  const { t } = useTranslation()

  const divisionOptions = useMemo(
    () =>
      (scoreboard?.divisions ?? [])
        .filter((division) => division.id !== undefined && division.id !== null)
        .map((division) => ({ value: division.id!.toString(), label: division.name?.trim() || `#${division.id}` })),
    [scoreboard?.divisions]
  )

  const selectValue = divisionId === null ? 'all' : divisionId.toString()

  useEffect(() => {
    if (divisionId !== null && !divisionOptions.some((option) => Number(option.value) === divisionId)) {
      setDivisionId(null)
    }
  }, [divisionOptions, divisionId, setDivisionId])

  const filtered = useMemo(() => {
    const ranked =
      divisionId === null
        ? scoreboard?.items?.filter((item) => item.rank > 0)
        : scoreboard?.items?.filter((item) => (item.divisionId ?? null) === divisionId)
    const normalized = debouncedKeyword.trim().toLowerCase()
    return normalized ? ranked?.filter((item) => item.name?.toLowerCase().includes(normalized)) : ranked
  }, [scoreboard, divisionId, debouncedKeyword])

  useEffect(() => setPage(1), [divisionId, debouncedKeyword])

  const base = (activePage - 1) * ITEM_COUNT_PER_PAGE
  const currentItems = filtered?.slice(base, base + ITEM_COUNT_PER_PAGE)
  const [currentItem, setCurrentItem] = useState<ScoreboardItem | null>(null)
  const [itemDetailOpened, setItemDetailOpened] = useState(false)

  useEffect(() => {
    if (scoreboard) setBloodBonus(new BloodBonus(scoreboard.bloodBonus))
  }, [scoreboard])

  const divisionMap = useMemo(() => {
    const map = new Map<number, string>()
    scoreboard?.divisions?.forEach((division) => map.set(division.id, division.name.trim()))
    return map
  }, [scoreboard?.divisions])

  const bloodData = useBonusLabels(bloodBonus)

  const findMyTeam = () => {
    if (!myTeamName || !filtered) return
    const index = filtered.findIndex((item) => item.name === myTeamName)
    if (index < 0) return
    setPage(Math.floor(index / ITEM_COUNT_PER_PAGE) + 1)
    setHighlightedTeam(myTeamName)
    window.setTimeout(() => setHighlightedTeam(null), 2500)
  }

  return (
    <Paper shadow="xs" p="sm" withBorder>
      <Stack gap="sm">
        <Stack gap="xs">
          {divisionOptions.length > 0 && (
            <Select
              label={t('game.label.score_table.division', 'Division')}
              data={[{ value: 'all', label: t('game.label.score_table.all_teams') }, ...divisionOptions]}
              value={selectValue}
              onChange={(division) => {
                if (!division || division === 'all') setDivisionId(null)
                else {
                  const parsed = Number(division)
                  setDivisionId(Number.isNaN(parsed) ? null : parsed)
                }
              }}
              styles={{ input: { minHeight: 44 } }}
            />
          )}
          <Group gap="xs" wrap="nowrap" align="flex-end">
            <TextInput
              label={t('game.placeholder.search_team', 'Search teams')}
              value={keyword}
              onChange={(event) => setKeyword(event.currentTarget.value)}
              leftSection={<Icon path={mdiMagnify} size={0.9} />}
              style={{ flex: 1 }}
              styles={{ input: { minHeight: 44 } }}
            />
            {myTeamName && (
              <Button
                variant="light"
                px="sm"
                h={44}
                aria-label={t('game.button.find_my_team', 'Find my team')}
                leftSection={<Icon path={mdiCrosshairsGps} size={0.85} />}
                onClick={findMyTeam}
              >
                {t('game.button.mine', 'Mine')}
              </Button>
            )}
          </Group>
        </Stack>

        <Box
          pos="relative"
          maw="100%"
          style={{ overflowX: 'auto' }}
          tabIndex={0}
          aria-label={t('game.label.score_table.caption', 'Team rankings and total scores')}
        >
          <Table className={classes.table}>
            <Table.Caption className="app-sr-only">
              {t('game.label.score_table.caption', 'Team rankings and total scores')}
            </Table.Caption>
            <colgroup>
              <col style={{ width: RANK_WIDTH }} />
              <col />
              <col style={{ width: SOLVED_WIDTH }} />
              <col style={{ width: SCORE_WIDTH }} />
            </colgroup>
            <Table.Thead className={classes.thead}>
              <Table.Tr>
                <Table.Th scope="col" className={cx(classes.left, classes.theadHeader)}>
                  {divisionId === null
                    ? t('game.label.score_table.rank_total')
                    : t('game.label.score_table.rank_division')}
                </Table.Th>
                <Table.Th scope="col" className={cx(classes.left, classes.theadHeader)}>
                  {t('game.label.score_table.team')}
                </Table.Th>
                <Table.Th scope="col" className={cx(classes.left, classes.theadHeader)}>
                  {t('game.label.score_table.solved_count')}
                </Table.Th>
                <Table.Th scope="col" className={cx(classes.left, classes.theadHeader)}>
                  {t('game.label.score_table.score_total')}
                </Table.Th>
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>
              {scoreboard && currentItems?.length === 0 ? (
                <Table.Tr>
                  <Table.Td colSpan={4}>
                    <Text c="dimmed" ta="center" py="md">
                      {t('game.label.score_table.empty', 'No teams match these filters')}
                    </Text>
                  </Table.Td>
                </Table.Tr>
              ) : (
                scoreboard &&
                currentItems?.map((item, index) => (
                  <TableRow
                    key={base + index}
                    item={item}
                    displayRank={divisionId === null ? item.rank : (item.divisionRank ?? base + index + 1)}
                    highlighted={highlightedTeam === item.name}
                    onOpenDetail={() => {
                      setCurrentItem(item)
                      setItemDetailOpened(true)
                    }}
                  />
                ))
              )}
            </Table.Tbody>
          </Table>
        </Box>

        <Paper component="details" withBorder p="xs" radius="md">
          <Text
            component="summary"
            size="sm"
            fw={650}
            mih={44}
            style={{ cursor: 'pointer', display: 'flex', alignItems: 'center' }}
          >
            {t('game.content.scoreboard_bonus_legend', 'Scoring bonus legend')}
          </Text>
          <Group gap="xs" mt="xs">
            {BloodsTypes.map((type) => (
              <Badge key={type} variant="light" color="gray">
                {bloodData.get(type)?.name}: {bloodData.get(type)?.descr}
              </Badge>
            ))}
          </Group>
          <Text size="xs" c="dimmed" mt="xs">
            {t('game.content.scoreboard_note')}
          </Text>
        </Paper>

        <Group justify="space-between" wrap="wrap" gap="xs">
          <Text size="xs" c="dimmed">
            {t('game.content.scoreboard_tip')}
          </Text>
          <ScoreboardPagination
            value={activePage}
            onChange={setPage}
            total={Math.ceil((filtered?.length ?? 1) / ITEM_COUNT_PER_PAGE)}
          />
        </Group>
      </Stack>

      <MobileScoreboardItemModal
        scoreboard={scoreboard}
        divisionMap={divisionMap}
        bloodBonusMap={bloodData}
        opened={itemDetailOpened}
        withCloseButton
        size="40rem"
        onClose={() => setItemDetailOpened(false)}
        item={currentItem}
      />
    </Paper>
  )
}
