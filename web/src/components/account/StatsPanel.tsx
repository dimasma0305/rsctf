import {
  Badge,
  Box,
  Button,
  Center,
  Group,
  Paper,
  ScrollArea,
  SimpleGrid,
  Skeleton,
  Stack,
  Table,
  Text,
  ThemeIcon,
  Tooltip,
  UnstyledButton,
  VisuallyHidden,
  useMantineColorScheme,
  useMantineTheme,
} from '@mantine/core'
import {
  mdiAlertCircleOutline,
  mdiChevronRight,
  mdiFire,
  mdiMedal,
  mdiPuzzle,
  mdiRefresh,
  mdiStar,
  mdiTrophy,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import relativeTime from 'dayjs/plugin/relativeTime'
import type { EChartsOption } from 'echarts'
import { FC, useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useNavigate } from 'react-router'
import useSWR from 'swr'
import { EchartsContainer } from '@Components/charts/EchartsContainer'
import { useLanguage } from '@Utils/I18n'
import classes from '@Pages/account/Stats.module.css'

dayjs.extend(relativeTime)

interface UserStatsModel {
  totalSolves: number
  totalFirstBloods: number
  gamesParticipated: number
  solvesByCategory: Record<string, number>
  games: { gameId: number; gameTitle: string; endTimeUtc: string; solves: number }[]
}

const CATEGORY_COLORS: Record<string, string> = {
  Misc: 'gray',
  Crypto: 'violet',
  Pwn: 'red',
  Web: 'blue',
  Reverse: 'orange',
  Blockchain: 'teal',
  Forensics: 'green',
  Hardware: 'cyan',
  Mobile: 'pink',
  PPC: 'lime',
  AI: 'yellow',
  Pentest: 'indigo',
  OSINT: 'grape',
}

const catColor = (cat: string) => CATEGORY_COLORS[cat] ?? 'blue'

const fetcher = (url: string) =>
  fetch(url, { credentials: 'include' }).then((r) => {
    if (!r.ok) throw new Error(`Request failed with status ${r.status}`)
    return r.json()
  })

/** The user's CTF statistics, rendered as a self-contained panel (no page chrome). */
export const StatsPanel: FC = () => {
  const { t } = useTranslation()
  const { locale } = useLanguage()
  const theme = useMantineTheme()
  const { colorScheme } = useMantineColorScheme()
  const navigate = useNavigate()

  const { data: stats, error, isLoading, mutate } = useSWR<UserStatsModel>('/api/account/stats', fetcher)

  const sortedCategories = useMemo(
    () => Object.entries(stats?.solvesByCategory ?? {}).sort((a, b) => b[1] - a[1]),
    [stats]
  )

  const topCategory = sortedCategories[0]
  const bestGameId = useMemo(() => {
    if (!stats?.games.length) return null
    return stats.games.reduce((best, g) => (g.solves > best.solves ? g : best)).gameId
  }, [stats])

  const categoryChartOption = useMemo((): EChartsOption => {
    if (!sortedCategories.length) return {}
    const entries = [...sortedCategories].reverse()
    return {
      backgroundColor: 'transparent',
      tooltip: { trigger: 'axis', axisPointer: { type: 'shadow' } },
      grid: { left: 76, right: 32, top: 8, bottom: 8, containLabel: false },
      xAxis: {
        type: 'value',
        minInterval: 1,
        axisLine: { show: false },
        axisTick: { show: false },
        splitLine: { lineStyle: { opacity: 0.25 } },
      },
      yAxis: {
        type: 'category',
        data: entries.map(([cat]) => cat),
        axisLine: { show: false },
        axisTick: { show: false },
      },
      series: [
        {
          type: 'bar',
          barWidth: '60%',
          data: entries.map(([cat, count]) => ({
            value: count,
            itemStyle: {
              color: theme.colors[catColor(cat)][colorScheme === 'dark' ? 6 : 5],
              borderRadius: [0, 4, 4, 0],
            },
          })),
          label: { show: true, position: 'right', fontWeight: 600 },
        },
      ],
    }
  }, [sortedCategories, theme, colorScheme])

  if (isLoading) {
    return (
      <Stack gap="lg" w="100%">
        <SimpleGrid cols={{ base: 2, sm: 4 }} spacing="md">
          {Array.from({ length: 4 }).map((_, i) => (
            <Skeleton key={i} h={132} radius="md" />
          ))}
        </SimpleGrid>
        <Skeleton h={220} radius="md" />
        <Skeleton h={240} radius="md" />
      </Stack>
    )
  }

  if (error || !stats) {
    return (
      <Center py={64}>
        <Stack align="center" gap="md">
          <Icon path={mdiAlertCircleOutline} size={3} color={theme.colors.red[5]} />
          <Text c="dimmed">{t('account.stats.error', 'Failed to load your stats. Please try again.')}</Text>
          <Button variant="light" leftSection={<Icon path={mdiRefresh} size={0.9} />} onClick={() => mutate()}>
            {t('common.button.retry', 'Retry')}
          </Button>
        </Stack>
      </Center>
    )
  }

  const summaryCards = [
    {
      key: 'solves',
      icon: mdiPuzzle,
      color: 'teal',
      value: stats.totalSolves,
      label: t('account.stats.total_solves', 'Total Solves'),
    },
    {
      key: 'bloods',
      icon: mdiFire,
      color: 'orange',
      value: stats.totalFirstBloods,
      label: t('account.stats.first_bloods', 'First Bloods'),
    },
    {
      key: 'games',
      icon: mdiTrophy,
      color: 'blue',
      value: stats.gamesParticipated,
      label: t('account.stats.games', 'Games Played'),
    },
    {
      key: 'top',
      icon: mdiMedal,
      color: topCategory ? catColor(topCategory[0]) : 'gray',
      value: topCategory ? topCategory[0] : '—',
      sub: topCategory ? t('account.stats.solve_count', '{{count}} solves', { count: topCategory[1] }) : undefined,
      label: t('account.stats.top_category', 'Top Category'),
    },
  ]

  return (
    <Stack gap="lg" w="100%">
      {/* Summary cards */}
      <SimpleGrid cols={{ base: 2, sm: 4 }} spacing="md">
        {summaryCards.map((card) => (
          <Paper
            key={card.key}
            withBorder
            radius="md"
            p={0}
            className={classes.statCard}
            style={{ overflow: 'hidden' }}
          >
            <Box h={3} bg={`${card.color}.5`} />
            <Stack gap={6} p="md" align="center">
              <ThemeIcon size="xl" color={card.color} variant="light" radius="md">
                <Icon path={card.icon} size={1.2} />
              </ThemeIcon>
              <Text
                size={typeof card.value === 'number' ? '1.9rem' : '1.25rem'}
                fw={800}
                c={card.color}
                lineClamp={1}
                ta="center"
              >
                {card.value}
              </Text>
              <Text size="xs" c="dimmed" ta="center">
                {card.label}
              </Text>
              {card.sub && (
                <Text size="xs" c="dimmed" ta="center">
                  {card.sub}
                </Text>
              )}
            </Stack>
          </Paper>
        ))}
      </SimpleGrid>

      {/* Category breakdown */}
      {sortedCategories.length > 0 && (
        <Paper p="md" withBorder radius="md">
          <Text fw={600} mb="sm">
            {t('account.stats.by_category', 'Solves by Category')}
          </Text>
          <EchartsContainer
            option={categoryChartOption}
            style={{ height: Math.max(150, sortedCategories.length * 34) }}
          />
          <Group gap="xs" mt="sm">
            {sortedCategories.map(([cat, count]) => (
              <Badge key={cat} color={catColor(cat)} variant="light" radius="sm">
                {cat} · {count}
              </Badge>
            ))}
          </Group>
        </Paper>
      )}

      {/* Game history */}
      {stats.games.length > 0 && (
        <Paper p="md" withBorder radius="md">
          <Text fw={600} mb="sm">
            {t('account.stats.game_history', 'Game History')}
          </Text>
          <ScrollArea>
            <Table highlightOnHover verticalSpacing="sm" miw={420}>
              <Table.Caption>
                <VisuallyHidden>{t('account.stats.game_history', 'Game History')}</VisuallyHidden>
              </Table.Caption>
              <Table.Thead>
                <Table.Tr>
                  <Table.Th scope="col">{t('common.label.game')}</Table.Th>
                  <Table.Th scope="col">{t('common.label.time')}</Table.Th>
                  <Table.Th scope="col" ta="right">
                    {t('account.stats.solves', 'Solves')}
                  </Table.Th>
                  <Table.Th scope="col" w={32}>
                    <VisuallyHidden>{t('common.label.action', 'Action')}</VisuallyHidden>
                  </Table.Th>
                </Table.Tr>
              </Table.Thead>
              <Table.Tbody>
                {stats.games.map((g) => (
                  <Table.Tr key={g.gameId} className={classes.clickRow} onClick={() => navigate(`/games/${g.gameId}`)}>
                    <Table.Td>
                      <UnstyledButton
                        component={Link}
                        to={`/games/${g.gameId}`}
                        aria-label={t('account.stats.open_game', {
                          defaultValue: 'Open {{game}}',
                          game: g.gameTitle,
                        })}
                        onClick={(event) => event.stopPropagation()}
                      >
                        <Group gap="xs" wrap="nowrap">
                          <Icon
                            path={g.gameId === bestGameId ? mdiTrophy : mdiStar}
                            size={0.7}
                            color={g.gameId === bestGameId ? theme.colors.yellow[5] : theme.colors.gray[5]}
                          />
                          <Text size="sm" lineClamp={1} fw={g.gameId === bestGameId ? 600 : 400}>
                            {g.gameTitle}
                          </Text>
                        </Group>
                      </UnstyledButton>
                    </Table.Td>
                    <Table.Td>
                      <Tooltip label={dayjs(g.endTimeUtc).locale(locale).format('LLL')} withArrow openDelay={300}>
                        <Text size="xs" c="dimmed">
                          {dayjs(g.endTimeUtc).locale(locale).fromNow()}
                        </Text>
                      </Tooltip>
                    </Table.Td>
                    <Table.Td ta="right">
                      <Badge color="teal" variant="light" radius="sm">
                        {g.solves}
                      </Badge>
                    </Table.Td>
                    <Table.Td>
                      <Icon path={mdiChevronRight} size={0.8} color={theme.colors.gray[5]} />
                    </Table.Td>
                  </Table.Tr>
                ))}
              </Table.Tbody>
            </Table>
          </ScrollArea>
        </Paper>
      )}

      {/* Empty state */}
      {stats.totalSolves === 0 && (
        <Center py={48}>
          <Stack align="center" gap="sm">
            <Icon path={mdiPuzzle} size={3} color={theme.colors.gray[5]} />
            <Text c="dimmed">{t('account.stats.empty', 'No solves yet — go play some CTFs!')}</Text>
            <Button component={Link} to="/games" variant="light" leftSection={<Icon path={mdiTrophy} size={0.9} />}>
              {t('account.stats.browse_games', 'Browse Games')}
            </Button>
          </Stack>
        </Center>
      )}
    </Stack>
  )
}

export default StatsPanel
