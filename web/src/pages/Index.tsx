import { Anchor, Group, Skeleton, Stack, Text, Title } from '@mantine/core'
import { mdiFlagCheckered, mdiAccountGroupOutline, mdiBookOpenPageVariantOutline } from '@mdi/js'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { useSWRConfig } from 'swr'
import { Empty } from '@Components/Empty'
import { GameStatus } from '@Components/GameCard'
import { PageHeader } from '@Components/PageHeader'
import { PostCard } from '@Components/PostCard'
import { RecentGame } from '@Components/RecentGame'
import { WithNavBar } from '@Components/WithNavbar'
import { WorkspaceLinks } from '@Components/WorkspaceLinks'
import { RecentGameCarousel } from '@Components/mobile/RecentGameCarousel'
import { invalidatePostPageCaches, postFeedSWRConfig } from '@Utils/PostFeed'
import { useServerNow } from '@Utils/ServerClock'
import { showErrorMsg } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import { getGameStatus, useRecentGames } from '@Hooks/useGame'
import { usePageTitle } from '@Hooks/usePageTitle'
import api, { PostInfoModel } from '@Api'
import classes from '@Styles/Index.module.css'

const Home: FC = () => {
  const { t } = useTranslation()
  const { mutate: mutateCache } = useSWRConfig()
  const { data: posts, mutate } = api.info.useInfoGetLatestPosts(postFeedSWRConfig)
  const { recentGames } = useRecentGames()
  const now = useServerNow()
  const isMobile = useIsMobile(900)
  const showGames = isMobile ? recentGames : recentGames?.slice(0, 5)
  const liveCount = recentGames?.filter((game) => getGameStatus(game, now).status === GameStatus.OnGoing).length ?? 0
  const upcomingCount = recentGames?.filter((game) => getGameStatus(game, now).status === GameStatus.Coming).length ?? 0

  const onTogglePinned = async (post: PostInfoModel, setDisabled: (value: boolean) => void) => {
    setDisabled(true)
    try {
      await api.edit.editUpdatePost(post.id, { isPinned: !post.isPinned })
      await mutate()
      void api.info.mutateInfoGetPosts()
      void invalidatePostPageCaches(mutateCache)
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  usePageTitle()

  const gamesHeading = (
    <Group justify="space-between" align="center" className={classes.sectionHeader}>
      <Title id="recent-games-title" order={2} size="h4">
        {t('common.content.home.recent_games')}
      </Title>
      <Anchor component={Link} to="/games" size="sm" fw={600} className={classes.viewAllLink}>
        {t('common.button.view_all', 'View all')}
      </Anchor>
    </Group>
  )

  return (
    <WithNavBar withFooter withHeader stickyHeader>
      <Stack gap="md" className={classes.home} data-home-overview>
        <PageHeader
          title={t('common.content.home.title', 'Overview')}
          actions={
            <WorkspaceLinks
              label={t('common.workspace.start_here', 'Start here')}
              items={[
                { to: '/games', icon: mdiFlagCheckered, title: t('game.title.index') },
                { to: '/teams', icon: mdiAccountGroupOutline, title: t('team.title.index') },
                { to: '/guide', icon: mdiBookOpenPageVariantOutline, title: t('common.title.guide', 'Player guide') },
              ]}
            />
          }
        />

        {isMobile && (
          <div>
            {gamesHeading}
            {showGames === undefined ? (
              <Skeleton h={230} radius="md" />
            ) : showGames.length === 0 ? (
              <Text size="sm" c="dimmed" py="md">
                {t('common.content.home.no_recent_games', 'No recent games')}
              </Text>
            ) : (
              <RecentGameCarousel games={showGames} />
            )}
          </div>
        )}

        <div className={classes.dashboard}>
          <section className={classes.feed} aria-labelledby="news-feed-title">
            <Group justify="space-between" align="center" className={classes.sectionHeader}>
              <Title id="news-feed-title" order={2} size="h4">
                {t('common.content.home.news', 'News & announcements')}
              </Title>
              <Anchor component={Link} to="/posts" size="sm" fw={600} className={classes.viewAllLink}>
                {t('common.button.view_all', 'View all')}
              </Anchor>
            </Group>
            <Stack gap={0}>
              {!Array.isArray(posts) ? (
                Array.from({ length: 3 }).map((_, i) => (
                  <Stack key={i} gap={12} py="lg">
                    <Skeleton height={12} width="30%" radius="sm" />
                    <Skeleton height={20} width="70%" radius="sm" />
                    <Skeleton height={12} radius="sm" />
                  </Stack>
                ))
              ) : posts.length === 0 ? (
                <Empty title={t('post.content.empty_title', 'No announcements yet')} />
              ) : (
                posts.map((post) => (
                  <PostCard key={post.id} post={post} layout="feed" headingOrder={3} onTogglePinned={onTogglePinned} />
                ))
              )}
            </Stack>
          </section>

          {!isMobile && (
            <aside className={classes.games} aria-labelledby="recent-games-title">
              {gamesHeading}
              {(liveCount > 0 || upcomingCount > 0) && (
                <Group gap="md" py="sm">
                  {liveCount > 0 && (
                    <Text size="sm" c="dimmed">
                      {t('game.content.live_count', '{{count}} live', { count: liveCount })}
                    </Text>
                  )}
                  {upcomingCount > 0 && (
                    <Text size="sm" c="dimmed">
                      {t('game.content.upcoming_count', '{{count}} upcoming', { count: upcomingCount })}
                    </Text>
                  )}
                </Group>
              )}
              <Stack gap="sm" mt="sm">
                {showGames === undefined ? (
                  Array.from({ length: 3 }).map((_, index) => <Skeleton key={index} h={102} radius="md" />)
                ) : showGames.length === 0 ? (
                  <Text size="sm" c="dimmed" py="md">
                    {t('common.content.home.no_recent_games', 'No recent games')}
                  </Text>
                ) : (
                  showGames.map((game) => <RecentGame key={game.id} game={game} />)
                )}
              </Stack>
            </aside>
          )}
        </div>
      </Stack>
    </WithNavBar>
  )
}

export default Home
