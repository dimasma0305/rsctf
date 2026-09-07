import { Alert, Badge, Button, Group, Pagination, Skeleton, Stack, Text, Title } from '@mantine/core'
import {
  mdiAlertCircleOutline,
  mdiArrowRight,
  mdiBookOpenPageVariantOutline,
  mdiFlagOutline,
  mdiNewspaperVariantOutline,
  mdiPlus,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { useSWRConfig } from 'swr'
import { Empty } from '@Components/Empty'
import { PageHeader } from '@Components/PageHeader'
import { PostCard } from '@Components/PostCard'
import { WithNavBar } from '@Components/WithNavbar'
import { RequireRole } from '@Components/WithRole'
import { invalidatePostPageCaches } from '@Utils/PostFeed'
import { showErrorMsg } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import { OnceSWRConfig } from '@Hooks/useConfig'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useUserRole } from '@Hooks/useUser'
import api, { PostInfoModel, Role } from '@Api'
import classes from '@Styles/PostsIndex.module.css'

const ITEMS_PER_PAGE = 10

const Posts: FC = () => {
  const { mutate: mutateCache } = useSWRConfig()
  const [activePage, setPage] = useState(1)
  const pageQuery = useMemo(() => ({ count: ITEMS_PER_PAGE, skip: (activePage - 1) * ITEMS_PER_PAGE }), [activePage])
  const { data: postPage, error, isValidating, mutate } = api.info.useInfoGetPostsPage(pageQuery, OnceSWRConfig)
  const feedHeading = useRef<HTMLHeadingElement>(null)
  const posts = postPage?.data
  const pageCount = Math.max(1, Math.ceil((postPage?.total ?? 0) / ITEMS_PER_PAGE))

  const isMobile = useIsMobile()
  const { role } = useUserRole()

  const { t } = useTranslation()

  usePageTitle(t('post.title.index'))

  useEffect(() => {
    if (postPage && activePage > pageCount) setPage(pageCount)
  }, [activePage, pageCount, postPage])

  const changePage = (page: number) => {
    setPage(page)
    feedHeading.current?.focus({ preventScroll: true })
    feedHeading.current?.scrollIntoView({ block: 'start', behavior: 'instant' })
  }

  const onTogglePinned = async (post: PostInfoModel, setDisabled: (value: boolean) => void) => {
    setDisabled(true)

    try {
      await api.edit.editUpdatePost(post.id, {
        isPinned: !post.isPinned,
      })
      await invalidatePostPageCaches(mutateCache)
      void api.info.mutateInfoGetLatestPosts()
      void api.info.mutateInfoGetPosts()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  return (
    <WithNavBar withFooter>
      <PageHeader
        eyebrow={t('post.content.news', 'News & updates')}
        title={t('post.title.index')}
        description={t('post.content.index_description', 'Announcements, guides, and updates from the organizers.')}
        actions={
          RequireRole(Role.Admin, role) && (
            <Button
              component={Link}
              to="/posts/new/edit"
              leftSection={<Icon path={mdiPlus} size={0.8} aria-hidden="true" />}
            >
              {t('post.button.new')}
            </Button>
          )
        }
      />
      <div className={classes.workspace} data-posts-workspace>
        <section className={classes.feed} aria-labelledby="post-feed-title" aria-busy={!postPage && !error}>
          <Group justify="space-between" gap="sm" className={classes.feedHeader}>
            <Title order={2} ref={feedHeading} tabIndex={-1} id="post-feed-title" className={classes.feedTitle}>
              {t('post.content.latest', 'Latest updates')}
            </Title>
            {postPage && (
              <Badge variant="light" color="gray" className={classes.count}>
                {t('post.content.total', '{{count}} posts', { count: postPage.total })}
              </Badge>
            )}
          </Group>
          {error && (
            <Alert
              icon={<Icon path={mdiAlertCircleOutline} size={0.9} />}
              color="red"
              title={t('post.content.load_error', 'Posts could not be loaded')}
              role="alert"
            >
              <Text size="sm">{t('post.content.retry_hint', 'Your place is saved. Try loading this page again.')}</Text>
              <Button
                variant="light"
                color="red"
                size="xs"
                mt="sm"
                loading={isValidating}
                onClick={() => void mutate()}
              >
                {t('common.button.retry', 'Try again')}
              </Button>
            </Alert>
          )}
          {!postPage && !error ? (
            <Stack role="status" aria-label={t('post.content.loading', 'Loading posts')} data-posts-loading>
              {[0, 1, 2].map((item) => (
                <div key={item} className={classes.skeleton} aria-hidden="true">
                  <Skeleton height={16} width="34%" animate={false} />
                  <Skeleton height={24} width="80%" mt="md" animate={false} />
                  <Skeleton height={12} mt="lg" animate={false} />
                  <Skeleton height={12} width="65%" mt="xs" animate={false} />
                </div>
              ))}
            </Stack>
          ) : posts?.length === 0 ? (
            <Empty
              bordered
              mdiPath={mdiNewspaperVariantOutline}
              title={t('post.content.empty_title', 'The notice board is quiet')}
              description={t('post.content.empty', 'No posts have been published yet.')}
              action={
                <Button component={Link} to="/games" variant="light">
                  {t('post.content.browse_events', 'Browse events')}
                </Button>
              }
            />
          ) : (
            <Stack gap="md">
              {posts?.map((post) => (
                <PostCard key={post.id} post={post} headingOrder={3} onTogglePinned={onTogglePinned} />
              ))}
            </Stack>
          )}
          {postPage && pageCount > 1 && (
            <nav aria-label={t('post.content.pagination_label', 'News result pages')} className={classes.paginationNav}>
              <Pagination.Root total={pageCount} siblings={isMobile ? 0 : 2} value={activePage} onChange={changePage}>
                <Group gap={5} justify={isMobile ? 'center' : 'flex-end'}>
                  {!isMobile && <Pagination.First aria-label={t('common.pagination.first', 'First page')} />}
                  <Pagination.Previous aria-label={t('common.pagination.previous', 'Previous page')} />
                  <Pagination.Items />
                  <Pagination.Next aria-label={t('common.pagination.next', 'Next page')} />
                  {!isMobile && <Pagination.Last aria-label={t('common.pagination.last', 'Last page')} />}
                </Group>
              </Pagination.Root>
            </nav>
          )}
        </section>
        <aside className={classes.sidebar} aria-labelledby="post-resources-title">
          <div className={classes.boardNote}>
            <Icon path={mdiNewspaperVariantOutline} size={1.6} aria-hidden="true" />
            <Text className={classes.kicker}>{t('post.content.notice_board', 'Notice board')}</Text>
            <Title order={2} id="post-resources-title" className={classes.sidebarTitle}>
              {t('post.content.stay_in_loop', 'Before your next challenge.')}
            </Title>
            <Text size="sm" c="dimmed">
              {t(
                'post.content.board_description',
                'Check here for organizer announcements, event updates, and things worth knowing before you play.'
              )}
            </Text>
          </div>
          <nav aria-label={t('post.content.explore', 'Explore the platform')} className={classes.links}>
            <Link to="/games" className={classes.resourceLink}>
              <Icon path={mdiFlagOutline} size={0.95} aria-hidden="true" />
              <span>
                <strong>{t('post.content.browse_events', 'Browse events')}</strong>
                <small>{t('post.content.events_hint', 'Find your next competition')}</small>
              </span>
              <Icon path={mdiArrowRight} size={0.8} aria-hidden="true" />
            </Link>
            <Link to="/guide" className={classes.resourceLink}>
              <Icon path={mdiBookOpenPageVariantOutline} size={0.95} aria-hidden="true" />
              <span>
                <strong>{t('post.content.player_guide', 'Player guide')}</strong>
                <small>{t('post.content.guide_hint', 'Get ready to play')}</small>
              </span>
              <Icon path={mdiArrowRight} size={0.8} aria-hidden="true" />
            </Link>
          </nav>
        </aside>
      </div>
    </WithNavBar>
  )
}

export default Posts
