import { Alert, Anchor, Box, Button, Group, Skeleton, Stack, Text, Title } from '@mantine/core'
import {
  mdiFlagCheckered,
  mdiAccountGroupOutline,
  mdiBookOpenPageVariantOutline,
  mdiAlertCircleOutline,
  mdiPinOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
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
import { invalidatePostPageCaches, postFeedSWRConfig } from '@Utils/PostFeed'
import { useServerNow } from '@Utils/ServerClock'
import { showErrorMsg } from '@Utils/Shared'
import { getGameStatus, useRecentGames } from '@Hooks/useGame'
import { usePageTitle } from '@Hooks/usePageTitle'
import api, { PostInfoModel } from '@Api'
import classes from '@Styles/Index.module.css'

const Home: FC = () => {
  const { t } = useTranslation()
  const { mutate: mutateCache } = useSWRConfig()
  const { data: posts, error: postsError, isValidating, mutate } = api.info.useInfoGetLatestPosts(postFeedSWRConfig)
  const { recentGames, error: gamesError, mutate: retryGames } = useRecentGames()
  const now = useServerNow()
  const eventPriority = (game: NonNullable<typeof recentGames>[number]) => {
    const status = getGameStatus(game, now).status
    return (status === GameStatus.OnGoing ? 0 : status === GameStatus.Coming ? 2 : 4) + (game.joined ? 0 : 1)
  }
  const showGames = recentGames
    ? [...recentGames].sort((a, b) => eventPriority(a) - eventPriority(b)).slice(0, 5)
    : undefined
  const pinnedPost = Array.isArray(posts) ? posts.find((post) => post.isPinned) : undefined
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
        {t('interface.home.recent_events')}
      </Title>
      <Anchor component={Link} to="/games" size="sm" fw={600} className={classes.viewAllLink}>
        {t('interface.home.view_all')}
      </Anchor>
    </Group>
  )

  return (
    <WithNavBar withFooter withHeader stickyHeader>
      <Stack gap="md" className={classes.home} data-home-overview>
        <PageHeader
          title={t('common.content.home.title', 'Overview')}
          actions={
            <Box className={classes.shortcuts}>
              <WorkspaceLinks
                label={t('common.workspace.start_here', 'Start here')}
                items={[
                  { to: '/games', icon: mdiFlagCheckered, title: t('interface.navigation.events') },
                  { to: '/teams', icon: mdiAccountGroupOutline, title: t('common.tab.team') },
                  { to: '/guide', icon: mdiBookOpenPageVariantOutline, title: t('interface.home.player_guide') },
                ]}
              />
            </Box>
          }
        />

        <aside className={classes.pinnedNotice} data-home-pinned aria-label={t('common.tab.post')}>
          <Icon path={mdiPinOutline} size={0.9} aria-hidden="true" />
          <div>
            <Text size="xs" fw={650} c="dimmed">
              {pinnedPost ? t('interface.home.pinned') : t('common.tab.post')}
            </Text>
            <Anchor
              component={Link}
              to={pinnedPost ? `/posts/${pinnedPost.id}` : '/posts'}
              fw={650}
              c="var(--app-accent-surface-text)"
            >
              {pinnedPost?.title ?? t('common.tab.post')}
            </Anchor>
          </div>
        </aside>

        <div className={classes.dashboard}>
          <aside
            className={classes.games}
            aria-labelledby="recent-games-title"
            data-home-recent
            aria-busy={(!showGames && !gamesError) || undefined}
          >
            {gamesHeading}
            <Group gap="md" py="sm">
              <Text size="sm" c="dimmed">
                {t('interface.home.live_count', { count: liveCount })}
              </Text>
              <Text size="sm" c="dimmed">
                {t('interface.home.upcoming_count', { count: upcomingCount })}
              </Text>
            </Group>
            {gamesError && (
              <Alert
                mt="sm"
                color="red"
                role="alert"
                title={t('game.content.load_failed', 'Events could not be loaded')}
              >
                <Button variant="outline" onClick={() => void retryGames().catch(() => undefined)}>
                  {t('common.button.retry', 'Retry')}
                </Button>
              </Alert>
            )}
            <Stack gap="sm" mt="sm" className={classes.recentList}>
              {showGames === undefined ? (
                !gamesError && Array.from({ length: 3 }).map((_, index) => <Skeleton key={index} h={102} radius="md" />)
              ) : showGames.length === 0 ? (
                <Text size="sm" c="dimmed" py="md">
                  {t('common.content.home.no_recent_games', 'No recent games')}
                </Text>
              ) : (
                showGames.map((game) => <RecentGame key={game.id} game={game} />)
              )}
            </Stack>
          </aside>

          <section
            className={classes.feed}
            aria-labelledby="news-feed-title"
            aria-busy={(!posts && !postsError) || undefined}
          >
            <Group justify="space-between" align="center" className={classes.sectionHeader}>
              <Title id="news-feed-title" order={2} size="h4">
                {t('interface.home.announcements')}
              </Title>
              <Anchor component={Link} to="/posts" size="sm" fw={600} className={classes.viewAllLink}>
                {t('interface.home.view_all')}
              </Anchor>
            </Group>
            {postsError && (
              <Alert
                mt="sm"
                color="red"
                icon={<Icon path={mdiAlertCircleOutline} size={0.9} aria-hidden="true" />}
                title={t('common.content.home.news_unavailable', 'Announcements could not be refreshed')}
                role="alert"
              >
                <Text size="sm">{t('common.content.home.retry_hint', 'Check your connection and try again.')}</Text>
                <Button
                  mt="sm"
                  variant="outline"
                  loading={isValidating}
                  onClick={() => void mutate().catch(() => undefined)}
                >
                  {t('common.button.retry', 'Retry')}
                </Button>
              </Alert>
            )}
            <Stack gap={0}>
              {!Array.isArray(posts) ? (
                !postsError &&
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
        </div>
      </Stack>
    </WithNavBar>
  )
}

export default Home
