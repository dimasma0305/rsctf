import { Anchor, Badge, Button, Group, Paper, Skeleton, Stack, Text, ThemeIcon, Title } from '@mantine/core'
import { mdiArrowRight, mdiFlagCheckered, mdiNewspaperVariantOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { Empty } from '@Components/Empty'
import { GameStatus } from '@Components/GameCard'
import { PageHeader } from '@Components/PageHeader'
import { PostCard } from '@Components/PostCard'
import { RecentGame } from '@Components/RecentGame'
import { WithNavBar } from '@Components/WithNavbar'
import { MobilePostCard } from '@Components/mobile/PostCard'
import { RecentGameCarousel } from '@Components/mobile/RecentGameCarousel'
import { showErrorMsg } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import { getGameStatus, useRecentGames } from '@Hooks/useGame'
import { usePageTitle } from '@Hooks/usePageTitle'
import api, { PostInfoModel } from '@Api'
import classes from '@Styles/Index.module.css'

const Home: FC = () => {
  const { t } = useTranslation()
  const { data: posts, mutate } = api.info.useInfoGetLatestPosts({ refreshInterval: 5 * 60 * 1000 })
  const { recentGames } = useRecentGames()
  const isMobile = useIsMobile(900)
  const showGames = isMobile ? recentGames : recentGames?.slice(0, 5)
  const liveCount = recentGames?.filter((game) => getGameStatus(game).status === GameStatus.OnGoing).length ?? 0
  const upcomingCount = recentGames?.filter((game) => getGameStatus(game).status === GameStatus.Coming).length ?? 0

  const onTogglePinned = async (post: PostInfoModel, setDisabled: (value: boolean) => void) => {
    setDisabled(true)

    try {
      const res = await api.edit.editUpdatePost(post.id, { isPinned: !post.isPinned })
      if (post.isPinned) {
        mutate([
          ...(posts?.filter((p) => p.id !== post.id && p.isPinned) ?? []),
          { ...res.data },
          ...(posts?.filter((p) => p.id !== post.id && !p.isPinned) ?? []),
        ])
      } else {
        mutate([
          { ...res.data },
          ...(posts?.filter((p) => p.id !== post.id && p.isPinned) ?? []),
          ...(posts?.filter((p) => p.id !== post.id && !p.isPinned) ?? []),
        ])
      }
      api.info.mutateInfoGetPosts()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  usePageTitle()

  return (
    <WithNavBar withFooter withHeader stickyHeader>
      <Stack gap="lg" className={classes.home}>
        <PageHeader
          eyebrow={t('common.content.home.eyebrow', 'Command center')}
          title={t('common.content.home.title', 'Latest updates')}
          description={t(
            'common.content.home.description',
            'Catch up on platform news and jump back into your recent competitions.'
          )}
          actions={
            <Stack gap="sm" align={isMobile ? 'stretch' : 'flex-end'}>
              {!!recentGames?.length && (
                <Group gap={6} justify={isMobile ? 'flex-start' : 'flex-end'}>
                  <Badge color="green" variant="light" size="lg">
                    {t('game.content.live_count', '{{count}} live', { count: liveCount })}
                  </Badge>
                  <Badge color="yellow" variant="light" size="lg">
                    {t('game.content.upcoming_count', '{{count}} upcoming', { count: upcomingCount })}
                  </Badge>
                </Group>
              )}
              <Button component={Link} to="/games" rightSection={<Icon path={mdiArrowRight} size={0.8} />}>
                {t('common.content.home.explore_games', 'Explore games')}
              </Button>
            </Stack>
          }
        />

        {isMobile && (
          <section aria-labelledby="competition-radar-title">
            <Group justify="space-between" align="center" mb="sm">
              <Group gap="sm">
                <ThemeIcon variant="light" size="lg" radius="md">
                  <Icon path={mdiFlagCheckered} size={0.9} aria-hidden="true" />
                </ThemeIcon>
                <div>
                  <Text size="xs" c="dimmed" fw={750} tt="uppercase" className={classes.sectionEyebrow}>
                    {t('common.content.home.competition_radar', 'Competition radar')}
                  </Text>
                  <Title id="competition-radar-title" order={2} size="h3">
                    {t('common.content.home.recent_games')}
                  </Title>
                </div>
              </Group>
              <Anchor component={Link} to="/games" size="sm" fw={650} className={classes.viewAllLink}>
                {t('common.button.view_all', 'View all')}
              </Anchor>
            </Group>

            {showGames === undefined ? (
              <Skeleton h={230} radius="lg" />
            ) : showGames.length === 0 ? (
              <Empty bordered description={t('common.content.home.no_recent_games', 'No recent games')} />
            ) : (
              <RecentGameCarousel games={showGames} />
            )}
          </section>
        )}

        <div className={classes.dashboard}>
          <Paper
            component="section"
            withBorder
            p={{ base: 'md', sm: 'lg' }}
            className={classes.feed}
            aria-labelledby="news-feed-title"
          >
            <Group justify="space-between" align="center" mb="md">
              <Group gap="sm">
                <ThemeIcon variant="light" size="lg" radius="md">
                  <Icon path={mdiNewspaperVariantOutline} size={0.9} aria-hidden="true" />
                </ThemeIcon>
                <div>
                  <Text size="xs" c="dimmed" fw={750} tt="uppercase" className={classes.sectionEyebrow}>
                    {t('common.content.home.platform_feed', 'Platform feed')}
                  </Text>
                  <Title id="news-feed-title" order={2} size="h3">
                    {t('common.content.home.news', 'News & announcements')}
                  </Title>
                </div>
              </Group>
              {Array.isArray(posts) && posts.length > 0 && (
                <Badge variant="light" color="gray">
                  {posts.length}
                </Badge>
              )}
            </Group>

            <Stack className={classes.posts} gap="md">
              {!Array.isArray(posts) ? (
                Array.from({ length: 3 }).map((_, i) => (
                  <Stack key={i} gap={8} p="md">
                    <Group>
                      <Skeleton height={32} circle />
                      <Skeleton height={12} width="30%" radius="sm" />
                    </Group>
                    <Skeleton height={20} width="70%" radius="sm" />
                    <Skeleton height={12} radius="sm" />
                    <Skeleton height={12} width="90%" radius="sm" />
                    <Skeleton height={12} width="60%" radius="sm" />
                  </Stack>
                ))
              ) : posts.length === 0 ? (
                <Empty
                  bordered
                  title={t('post.content.empty_title', 'No announcements yet')}
                  description={t(
                    'post.content.empty_description',
                    'There is nothing to catch up on. Explore the competition schedule while you wait.'
                  )}
                  action={
                    <Button
                      component={Link}
                      to="/games"
                      variant="light"
                      rightSection={<Icon path={mdiArrowRight} size={0.75} />}
                    >
                      {t('common.content.home.browse_competitions', 'Browse competitions')}
                    </Button>
                  }
                />
              ) : isMobile ? (
                posts.map((post) => <MobilePostCard key={post.id} post={post} onTogglePinned={onTogglePinned} />)
              ) : (
                posts.map((post) => <PostCard key={post.id} post={post} onTogglePinned={onTogglePinned} />)
              )}
            </Stack>
          </Paper>

          {!isMobile && (
            <Paper component="aside" withBorder p="md" className={classes.games} aria-labelledby="recent-games-title">
              <Group justify="space-between" align="center" mb="md">
                <Group gap="sm" wrap="nowrap">
                  <ThemeIcon variant="light" size="lg" radius="md">
                    <Icon path={mdiFlagCheckered} size={0.9} aria-hidden="true" />
                  </ThemeIcon>
                  <div>
                    <Text size="xs" c="dimmed" fw={750} tt="uppercase" className={classes.sectionEyebrow}>
                      {t('common.content.home.competition_radar', 'Competition radar')}
                    </Text>
                    <Title id="recent-games-title" order={2} size="h3">
                      {t('common.content.home.recent_games')}
                    </Title>
                  </div>
                </Group>
                <Anchor component={Link} to="/games" size="sm" fw={650} className={classes.viewAllLink}>
                  {t('common.button.view_all', 'View all')}
                </Anchor>
              </Group>

              <Stack gap="sm">
                {showGames === undefined ? (
                  Array.from({ length: 3 }).map((_, index) => <Skeleton key={index} h={102} radius="md" />)
                ) : showGames.length === 0 ? (
                  <Empty bordered description={t('common.content.home.no_recent_games', 'No recent games')} />
                ) : (
                  showGames.map((game) => <RecentGame key={game.id} game={game} />)
                )}
              </Stack>
            </Paper>
          )}
        </div>
      </Stack>
    </WithNavBar>
  )
}

export default Home
