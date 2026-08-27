import {
  Card,
  Grid,
  Group,
  ScrollArea,
  Stack,
  Table,
  Tabs,
  Text,
  ThemeIcon,
  Title,
  Skeleton,
  Badge,
  ActionIcon,
  Avatar,
  SegmentedControl,
  useMantineColorScheme,
  useMantineTheme,
} from '@mantine/core'
import {
  mdiAccountGroup,
  mdiAccountMultiple,
  mdiDocker,
  mdiThumbUp,
  mdiThumbDown,
  mdiArrowLeftBold,
  mdiArrowRightBold,
  mdiRefresh,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import type { EChartsOption } from 'echarts'
import { FC, useCallback, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import useSWR from 'swr'
import { Empty } from '@Components/Empty'
import { ScrollingText } from '@Components/ScrollingText'
import { AdminPage } from '@Components/admin/AdminPage'
import { EchartsContainer } from '@Components/charts/EchartsContainer'
import { startAdminDashboardRefresh } from '@Utils/AdminDashboardRefresh'
import { showErrorMsg } from '@Utils/Shared'
import api, {
  AdminDashboardModel,
  ChallengeReviewDetailModel,
  CheatInfoModel,
  WriteupInfo,
  SubmissionTrendModel,
} from '@Api'
import classes from '@Styles/AdminDashboard.module.css'

const STATS_ICON_SIZE = 1.5
const TABLE_PAGE_SIZE = 10
const DASHBOARD_SWR_CONFIG = {
  refreshInterval: 0,
  revalidateOnFocus: false,
  revalidateOnReconnect: false,
} as const
type ActivityTab = 'reviews' | 'writeups' | 'cheats'

const StatCard: FC<{
  title: string
  value?: number
  icon: string
  color: string
  to: string
  loading?: boolean
}> = ({ title, value, icon, color, to, loading }) => (
  <Card
    component={Link}
    to={to}
    withBorder
    padding="lg"
    radius="lg"
    className={classes.statCard}
    aria-label={loading ? title : `${title}: ${value ?? 0}`}
    aria-busy={loading || undefined}
  >
    <Group justify="space-between">
      <Stack gap={0}>
        <Text size="xs" c="dimmed" fw={700} tt="uppercase">
          {title}
        </Text>
        {loading ? (
          <Skeleton height={28} width={50} mt={5} radius="sm" />
        ) : (
          <Text fw={760} size="xl" className={classes.statValue}>
            {value ?? 0}
          </Text>
        )}
      </Stack>
      <ThemeIcon radius="md" size="lg" variant="light" color={color}>
        <Icon path={icon} size={STATS_ICON_SIZE} />
      </ThemeIcon>
    </Group>
  </Card>
)

const Dashboard: FC = () => {
  const { t } = useTranslation()
  const theme = useMantineTheme()
  const { colorScheme } = useMantineColorScheme()
  const [trendRange, setTrendRange] = useState<string>('Day')
  const [activityTab, setActivityTab] = useState<ActivityTab>('reviews')

  const {
    data: dashboard,
    error,
    isLoading,
    isValidating: isDashboardValidating,
    mutate: refreshDashboard,
  } = useSWR<AdminDashboardModel>(
    '/api/admin/dashboard',
    () => api.admin.adminGetDashboard().then((r) => r.data),
    DASHBOARD_SWR_CONFIG
  )

  const {
    data: trends,
    isLoading: isTrendLoading,
    isValidating: isTrendValidating,
    mutate: refreshTrend,
  } = useSWR<SubmissionTrendModel[]>(
    `/api/admin/submissiontrend?range=${trendRange}`,
    () => api.admin.adminGetSubmissionTrend({ range: trendRange }).then((r) => r.data),
    DASHBOARD_SWR_CONFIG
  )

  if (error) showErrorMsg(error, t)

  // Trend Chart Option
  const trendOption: EChartsOption = {
    aria: {
      enabled: true,
      description: t('admin.dashboard.submission_trend_description', 'Submission volume over the selected time range.'),
    },
    tooltip: { trigger: 'axis' },
    grid: { left: '3%', right: '4%', bottom: '3%', containLabel: true },
    xAxis: {
      type: 'category',
      boundaryGap: false,
      data:
        trends?.map((d) =>
          trendRange === 'Day' ? new Date(d.time).toLocaleTimeString() : new Date(d.time).toLocaleDateString()
        ) ?? [],
    },
    yAxis: { type: 'value' },
    series: [
      {
        name: t('admin.dashboard.submissions', 'Submissions'),
        type: 'line',
        stack: 'Total',
        areaStyle: {},
        data: trends?.map((d) => d.count) ?? [],
        smooth: true,
        showSymbol: false,
        color: theme.colors[theme.primaryColor][colorScheme === 'dark' ? 4 : 6],
      },
    ],
  }

  // Activity Tables State
  const [reviewPage, setReviewPage] = useState(1)
  const [writeupPage, setWriteupPage] = useState(1)
  const [cheatPage, setCheatPage] = useState(1)

  // Fetch Reviews
  const { data: reviewsData, mutate: refreshReviews } = useSWR<ChallengeReviewDetailModel[]>(
    activityTab === 'reviews' ? ['/api/admin/reviews', reviewPage] : null,
    () =>
      api.admin
        .adminGetReviews({ count: TABLE_PAGE_SIZE, skip: (reviewPage - 1) * TABLE_PAGE_SIZE })
        .then((r) => (Array.isArray(r.data) ? r.data : (r.data as any).data)),
    DASHBOARD_SWR_CONFIG
  )

  // Fetch Writeups
  const { data: writeupsData, mutate: refreshWriteups } = useSWR<WriteupInfo[]>(
    activityTab === 'writeups' ? ['/api/admin/writeups', writeupPage] : null,
    () =>
      api.admin
        .adminGetAllWriteups({ count: TABLE_PAGE_SIZE, skip: (writeupPage - 1) * TABLE_PAGE_SIZE })
        .then((r) => (Array.isArray(r.data) ? r.data : (r.data as any).data)),
    DASHBOARD_SWR_CONFIG
  )

  // Fetch Cheat Reports
  const { data: cheatsData, mutate: refreshCheats } = useSWR<CheatInfoModel[]>(
    activityTab === 'cheats' ? ['/api/admin/cheat-reports', cheatPage] : null,
    () =>
      api.admin
        .adminGetCheatReports({ count: TABLE_PAGE_SIZE, skip: (cheatPage - 1) * TABLE_PAGE_SIZE })
        .then((r) => (Array.isArray(r.data) ? r.data : (r.data as any).data)),
    DASHBOARD_SWR_CONFIG
  )

  const refreshVisibleDashboard = useCallback(async () => {
    const refreshActivity =
      activityTab === 'reviews' ? refreshReviews : activityTab === 'writeups' ? refreshWriteups : refreshCheats
    await Promise.all([refreshDashboard(), refreshTrend(), refreshActivity()])
  }, [activityTab, refreshCheats, refreshDashboard, refreshReviews, refreshTrend, refreshWriteups])

  useEffect(
    () =>
      startAdminDashboardRefresh({
        refresh: refreshVisibleDashboard,
        onError: (error) => showErrorMsg(error, t),
        isActive: () => document.visibilityState === 'visible' && document.hasFocus() && window.navigator.onLine,
        subscribe: (listener) => {
          document.addEventListener('visibilitychange', listener)
          window.addEventListener('focus', listener)
          window.addEventListener('blur', listener)
          window.addEventListener('online', listener)
          window.addEventListener('offline', listener)
          return () => {
            document.removeEventListener('visibilitychange', listener)
            window.removeEventListener('focus', listener)
            window.removeEventListener('blur', listener)
            window.removeEventListener('online', listener)
            window.removeEventListener('offline', listener)
          }
        },
        setTimer: window.setTimeout.bind(window),
        clearTimer: window.clearTimeout.bind(window),
      }),
    [refreshVisibleDashboard, t]
  )

  const SimplePagination = ({
    page,
    setPage,
    currentLength,
  }: {
    page: number
    setPage: (p: number) => void
    currentLength: number
  }) => (
    <Group justify="flex-end" mt="md">
      <ActionIcon
        size="lg"
        disabled={page <= 1}
        aria-label={t('common.pagination.previous', 'Previous page')}
        onClick={() => setPage(page - 1)}
      >
        <Icon path={mdiArrowLeftBold} size={1} />
      </ActionIcon>
      <Text fw="bold" size="sm">
        {page}
      </Text>
      <ActionIcon
        size="lg"
        disabled={currentLength < TABLE_PAGE_SIZE}
        aria-label={t('common.pagination.next', 'Next page')}
        onClick={() => setPage(page + 1)}
      >
        <Icon path={mdiArrowRightBold} size={1} />
      </ActionIcon>
    </Group>
  )

  return (
    <AdminPage isLoading={isLoading && !dashboard}>
      <Stack gap="md">
        <Group justify="flex-end">
          <ActionIcon
            variant="subtle"
            size="lg"
            loading={isDashboardValidating || isTrendValidating}
            aria-label={t('admin.dashboard.refresh', 'Refresh dashboard')}
            onClick={() => void refreshVisibleDashboard().catch((error) => showErrorMsg(error, t))}
          >
            <Icon path={mdiRefresh} size={1} aria-hidden />
          </ActionIcon>
        </Group>
        {/* Stats Row */}
        <Grid>
          <Grid.Col span={{ base: 12, sm: 6, md: 4 }}>
            <StatCard
              title={t('admin.dashboard.users', 'Users')}
              value={dashboard?.systemStats.userCount}
              icon={mdiAccountMultiple}
              color="blue"
              to="/admin/users"
              loading={isLoading}
            />
          </Grid.Col>
          <Grid.Col span={{ base: 12, sm: 6, md: 4 }}>
            <StatCard
              title={t('admin.dashboard.teams', 'Teams')}
              value={dashboard?.systemStats.teamCount}
              icon={mdiAccountGroup}
              color="cyan"
              to="/admin/teams"
              loading={isLoading}
            />
          </Grid.Col>
          <Grid.Col span={{ base: 12, sm: 6, md: 4 }}>
            <StatCard
              title={t('admin.dashboard.containers', 'Containers')}
              value={dashboard?.systemStats.activeContainerCount}
              icon={mdiDocker}
              color="indigo"
              to="/admin/instances"
              loading={isLoading}
            />
          </Grid.Col>
        </Grid>

        <Grid>
          {/* Trend Chart */}
          <Grid.Col span={{ base: 12, lg: 7 }}>
            <Card withBorder radius="lg" p="lg">
              <Group justify="space-between" mb="md">
                <Title order={2} size="h4">
                  {t('admin.dashboard.submission_trend', 'Submission Trend')}
                </Title>
                <SegmentedControl
                  size="xs"
                  aria-label={t('admin.dashboard.submission_trend_range', 'Submission trend range')}
                  value={trendRange}
                  onChange={setTrendRange}
                  data={[
                    { label: t('common.range.day', 'Day'), value: 'Day' },
                    { label: t('common.range.week', 'Week'), value: 'Week' },
                    { label: t('common.range.month', 'Month'), value: 'Month' },
                    { label: t('common.range.year', 'Year'), value: 'Year' },
                  ]}
                />
              </Group>
              {isTrendLoading ? (
                <Skeleton h={300} radius="md" />
              ) : trends?.some((point) => point.count > 0) ? (
                <EchartsContainer
                  option={trendOption}
                  aria-label={t('admin.dashboard.submission_trend', 'Submission Trend')}
                  style={{ height: 300, width: '100%' }}
                />
              ) : (
                <Empty
                  bordered
                  title={t('admin.dashboard.no_submission_activity', 'No submission activity')}
                  description={t(
                    'admin.dashboard.no_submission_activity_description',
                    'Submissions will appear here when competitors begin solving challenges in this time range.'
                  )}
                />
              )}
            </Card>
          </Grid.Col>

          {/* Popular Games */}
          <Grid.Col span={{ base: 12, lg: 5 }}>
            <Card withBorder radius="lg" p="lg" h="100%" className={classes.popularGamesCard}>
              <Title order={2} size="h4" mb="md">
                {t('admin.dashboard.popular_games', 'Popular Games')}
              </Title>
              <Stack gap="xs" hiddenFrom="sm">
                {dashboard?.topGames?.map((game) => (
                  <Card
                    component={Link}
                    to={`/games/${game.id}`}
                    key={game.id}
                    withBorder
                    padding="sm"
                    radius="md"
                    className={classes.popularGameMobileRow}
                  >
                    <Group justify="space-between" align="flex-start" wrap="nowrap">
                      <Group gap="sm" align="flex-start" wrap="nowrap" miw={0}>
                        <Avatar imageProps={{ loading: 'lazy' }} src={game.poster} radius="sm" size="sm" />
                        <Stack gap={4} miw={0}>
                          <Text fw={700} size="sm" lineClamp={2} title={game.title ?? ''}>
                            {game.title}
                          </Text>
                          <Group gap="sm" wrap="wrap">
                            <Group
                              gap={4}
                              role="group"
                              aria-label={`${t('admin.dashboard.users', 'Users')}: ${game.userCount ?? 0}`}
                            >
                              <Icon path={mdiAccountMultiple} size={0.7} aria-hidden />
                              <Text size="xs">{game.userCount ?? 0}</Text>
                            </Group>
                            <Group
                              gap={4}
                              role="group"
                              aria-label={`${t('admin.dashboard.teams', 'Teams')}: ${game.teamCount ?? 0}`}
                            >
                              <Icon path={mdiAccountGroup} size={0.7} aria-hidden />
                              <Text size="xs">{game.teamCount ?? 0}</Text>
                            </Group>
                          </Group>
                        </Stack>
                      </Group>
                      <Stack gap={0} align="flex-end" miw="4rem">
                        <Text
                          size="sm"
                          fw={700}
                          c={
                            game.averageRating && game.averageRating > 0.5
                              ? 'teal'
                              : game.averageRating !== undefined && game.averageRating !== null
                                ? 'red'
                                : 'dimmed'
                          }
                        >
                          {game.averageRating !== undefined && game.averageRating !== null
                            ? game.averageRating === 1
                              ? '100%'
                              : `${Math.round(game.averageRating * 100)}%`
                            : '-'}
                        </Text>
                        <Text size="xs" c="dimmed">
                          {game.reviewCount ?? 0} {t('admin.dashboard.reviews', 'Reviews').toLocaleLowerCase()}
                        </Text>
                      </Stack>
                    </Group>
                  </Card>
                ))}
                {(dashboard?.topGames?.length ?? 0) === 0 && (
                  <Text ta="center" c="dimmed" py="md">
                    {t('common.content.no_data', 'No Data')}
                  </Text>
                )}
              </Stack>
              <ScrollArea
                type="auto"
                offsetScrollbars
                visibleFrom="sm"
                className={classes.popularGamesScroll}
                viewportProps={{
                  tabIndex: 0,
                  'aria-label': t('admin.dashboard.popular_games_scroll', 'Scrollable popular games table'),
                }}
              >
                <Table miw={360} horizontalSpacing="xs">
                  <Table.Caption className="app-sr-only">
                    {t('admin.dashboard.popular_games', 'Popular Games')}
                  </Table.Caption>
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th scope="col">{t('common.label.game')}</Table.Th>
                      <Table.Th scope="col">{t('admin.dashboard.participants', 'Participants')}</Table.Th>
                      <Table.Th scope="col">{t('admin.dashboard.reviews', 'Reviews')}</Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {dashboard?.topGames?.map((game) => (
                      <Table.Tr key={game.id}>
                        <Table.Td>
                          <Group gap="sm" wrap="nowrap">
                            <Avatar imageProps={{ loading: 'lazy' }} src={game.poster} radius="sm" size="sm" />
                            <ScrollingText text={game.title ?? ''} maw="7rem" />
                          </Group>
                        </Table.Td>
                        <Table.Td>
                          <Stack gap={0}>
                            <Group gap="xs">
                              <Icon path={mdiAccountMultiple} size={0.8} />
                              <Text size="sm">{game.userCount ?? 0}</Text>
                            </Group>
                            <Group gap="xs">
                              <Icon path={mdiAccountGroup} size={0.8} />
                              <Text size="sm">{game.teamCount ?? 0}</Text>
                            </Group>
                          </Stack>
                        </Table.Td>
                        <Table.Td>
                          <Group gap="xs">
                            <Text
                              size="sm"
                              fw="bold"
                              c={
                                game.averageRating && game.averageRating > 0.5
                                  ? 'teal'
                                  : game.averageRating !== undefined && game.averageRating !== null
                                    ? 'red'
                                    : 'dimmed'
                              }
                            >
                              {game.averageRating !== undefined && game.averageRating !== null
                                ? game.averageRating === 1
                                  ? '100%'
                                  : `${Math.round(game.averageRating * 100)}%`
                                : '-'}
                            </Text>
                            <Text size="xs" c="dimmed">
                              ({game.reviewCount ?? 0})
                            </Text>
                          </Group>
                        </Table.Td>
                      </Table.Tr>
                    ))}
                    {(dashboard?.topGames?.length ?? 0) === 0 && (
                      <Table.Tr>
                        <Table.Td colSpan={3} align="center" c="dimmed">
                          {t('common.content.no_data', 'No Data')}
                        </Table.Td>
                      </Table.Tr>
                    )}
                  </Table.Tbody>
                </Table>
              </ScrollArea>
            </Card>
          </Grid.Col>
        </Grid>

        {/* Recent Activity Tabs */}
        <Card withBorder radius="lg" p="lg">
          <Tabs value={activityTab} onChange={(value) => value && setActivityTab(value as ActivityTab)}>
            <Tabs.List>
              <Tabs.Tab value="reviews">{t('admin.dashboard.recent_reviews', 'Recent Reviews')}</Tabs.Tab>
              <Tabs.Tab value="writeups">{t('admin.dashboard.recent_writeups', 'Recent Writeups')}</Tabs.Tab>
              <Tabs.Tab value="cheats">{t('admin.dashboard.recent_cheats', 'Recent Cheat Reports')}</Tabs.Tab>
            </Tabs.List>

            <Tabs.Panel value="reviews" pt="xs">
              <ScrollArea
                viewportProps={{
                  tabIndex: 0,
                  'aria-label': t('admin.dashboard.recent_reviews_scroll', 'Scrollable recent challenge reviews'),
                }}
              >
                <Table striped highlightOnHover style={{ minWidth: 700 }}>
                  <Table.Caption className="app-sr-only">
                    {t('admin.dashboard.recent_reviews', 'Recent Reviews')}
                  </Table.Caption>
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th scope="col">{t('common.label.user')}</Table.Th>
                      <Table.Th scope="col">{t('common.label.game')}</Table.Th>
                      <Table.Th scope="col">{t('common.label.challenge')}</Table.Th>
                      <Table.Th scope="col">{t('common.label.rating')}</Table.Th>
                      <Table.Th scope="col">{t('common.label.comment')}</Table.Th>
                      <Table.Th scope="col">{t('common.label.time')}</Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {reviewsData?.map((r) => (
                      <Table.Tr key={r.id}>
                        <Table.Td>
                          <ScrollingText text={r.userName ?? ''} maw="10rem" />
                        </Table.Td>
                        <Table.Td>
                          <ScrollingText text={r.gameTitle ?? ''} maw="10rem" />
                        </Table.Td>
                        <Table.Td>
                          <ScrollingText text={r.challengeName ?? ''} maw="10rem" />
                        </Table.Td>
                        <Table.Td>
                          {r.rating === 2 && (
                            <Group gap={4} c="teal">
                              <Icon path={mdiThumbUp} size={0.8} />
                              <Text size="xs">{t('admin.dashboard.positive_review', 'Positive')}</Text>
                            </Group>
                          )}
                          {r.rating === 1 && (
                            <Group gap={4} c="red">
                              <Icon path={mdiThumbDown} size={0.8} />
                              <Text size="xs">{t('admin.dashboard.negative_review', 'Negative')}</Text>
                            </Group>
                          )}
                        </Table.Td>
                        <Table.Td>
                          <Text truncate maw={300}>
                            {r.comment}
                          </Text>
                        </Table.Td>
                        <Table.Td>{new Date(r.submitTimeUtc!).toLocaleString()}</Table.Td>
                      </Table.Tr>
                    ))}
                    {(reviewsData?.length ?? 0) === 0 && (
                      <Table.Tr>
                        <Table.Td colSpan={6} align="center" c="dimmed">
                          {t('common.content.no_data', 'No Data')}
                        </Table.Td>
                      </Table.Tr>
                    )}
                  </Table.Tbody>
                </Table>
                <SimplePagination page={reviewPage} setPage={setReviewPage} currentLength={reviewsData?.length ?? 0} />
              </ScrollArea>
            </Tabs.Panel>

            <Tabs.Panel value="writeups" pt="xs">
              <ScrollArea
                viewportProps={{
                  tabIndex: 0,
                  'aria-label': t('admin.dashboard.recent_writeups_scroll', 'Scrollable recent writeups'),
                }}
              >
                <Table striped highlightOnHover style={{ minWidth: 700 }}>
                  <Table.Caption className="app-sr-only">
                    {t('admin.dashboard.recent_writeups', 'Recent Writeups')}
                  </Table.Caption>
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th scope="col">{t('common.label.team')}</Table.Th>
                      <Table.Th scope="col">{t('common.label.game')}</Table.Th>
                      <Table.Th scope="col">{t('common.label.download')}</Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {writeupsData?.map((w) => (
                      <Table.Tr key={w.id}>
                        <Table.Td>
                          <ScrollingText text={w.team?.name ?? ''} maw="10rem" />
                        </Table.Td>
                        <Table.Td>
                          <ScrollingText text={w.gameTitle ?? ''} maw="15rem" />
                        </Table.Td>
                        <Table.Td>
                          {w.url && (
                            <Badge color="blue" component="a" href={w.url} target="_blank" rel="noreferrer">
                              {t('common.label.download', 'Download')}
                            </Badge>
                          )}
                        </Table.Td>
                      </Table.Tr>
                    ))}
                    {(writeupsData?.length ?? 0) === 0 && (
                      <Table.Tr>
                        <Table.Td colSpan={3} align="center" c="dimmed">
                          {t('common.content.no_data', 'No Data')}
                        </Table.Td>
                      </Table.Tr>
                    )}
                  </Table.Tbody>
                </Table>
                <SimplePagination
                  page={writeupPage}
                  setPage={setWriteupPage}
                  currentLength={writeupsData?.length ?? 0}
                />
              </ScrollArea>
            </Tabs.Panel>

            <Tabs.Panel value="cheats" pt="xs">
              <ScrollArea
                viewportProps={{
                  tabIndex: 0,
                  'aria-label': t('admin.dashboard.recent_cheats_scroll', 'Scrollable recent cheat reports'),
                }}
              >
                <Table striped highlightOnHover style={{ minWidth: 700 }}>
                  <Table.Caption className="app-sr-only">
                    {t('admin.dashboard.recent_cheats', 'Recent Cheat Reports')}
                  </Table.Caption>
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th scope="col">{t('common.label.user')}</Table.Th>
                      <Table.Th scope="col">{t('common.label.team')}</Table.Th>
                      <Table.Th scope="col">{t('common.label.challenge')}</Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {cheatsData?.map((c, idx) => (
                      <Table.Tr key={idx}>
                        <Table.Td>
                          <ScrollingText text={c.submission?.user ?? ''} maw="10rem" />
                        </Table.Td>
                        <Table.Td>
                          <ScrollingText text={c.submission?.team ?? ''} maw="10rem" />
                        </Table.Td>
                        <Table.Td>
                          <ScrollingText text={c.submission?.challenge ?? ''} maw="10rem" />
                        </Table.Td>
                      </Table.Tr>
                    ))}
                    {(cheatsData?.length ?? 0) === 0 && (
                      <Table.Tr>
                        <Table.Td colSpan={3} align="center" c="dimmed">
                          {t('common.content.no_data', 'No Data')}
                        </Table.Td>
                      </Table.Tr>
                    )}
                  </Table.Tbody>
                </Table>
                <SimplePagination page={cheatPage} setPage={setCheatPage} currentLength={cheatsData?.length ?? 0} />
              </ScrollArea>
            </Tabs.Panel>
          </Tabs>
        </Card>
      </Stack>
    </AdminPage>
  )
}

export default Dashboard
