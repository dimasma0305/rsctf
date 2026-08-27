import { Button, Group, Pagination, Stack } from '@mantine/core'
import { mdiPlus } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { Empty } from '@Components/Empty'
import { PageHeader } from '@Components/PageHeader'
import { PostCard } from '@Components/PostCard'
import { WithNavBar } from '@Components/WithNavbar'
import { RequireRole } from '@Components/WithRole'
import { MobilePostCard } from '@Components/mobile/PostCard'
import { invalidatePostPageCaches } from '@Utils/PostFeed'
import { showErrorMsg } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import { OnceSWRConfig } from '@Hooks/useConfig'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useUserRole } from '@Hooks/useUser'
import api, { PostInfoModel, Role } from '@Api'
import misc from '@Styles/Misc.module.css'
import classes from '@Styles/PostsIndex.module.css'

const ITEMS_PER_PAGE = 10

const Posts: FC = () => {
  const [activePage, setPage] = useState(1)
  const pageQuery = useMemo(() => ({ count: ITEMS_PER_PAGE, skip: (activePage - 1) * ITEMS_PER_PAGE }), [activePage])
  const { data: postPage } = api.info.useInfoGetPostsPage(pageQuery, OnceSWRConfig)
  const posts = postPage?.data
  const pageCount = Math.max(1, Math.ceil((postPage?.total ?? 0) / ITEMS_PER_PAGE))

  const isMobile = useIsMobile()
  const { role } = useUserRole()

  const { t } = useTranslation()

  usePageTitle(t('post.title.index'))

  useEffect(() => {
    if (activePage > pageCount) setPage(pageCount)
  }, [activePage, pageCount])

  const onTogglePinned = async (post: PostInfoModel, setDisabled: (value: boolean) => void) => {
    setDisabled(true)

    try {
      await api.edit.editUpdatePost(post.id, {
        isPinned: !post.isPinned,
      })
      await invalidatePostPageCaches()
      void api.info.mutateInfoGetLatestPosts()
      void api.info.mutateInfoGetPosts()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  return (
    <WithNavBar isLoading={!postPage} withHeader stickyHeader>
      <PageHeader
        eyebrow={t('post.content.news', 'News & updates')}
        title={t('post.title.index')}
        description={t('post.content.index_description', 'Announcements, guides, and updates from the organizers.')}
      />
      <Stack justify="space-between" mih="calc(100vh - 78px)" mt={{ base: 'md', sm: 'lg' }}>
        <Stack>
          {posts?.length === 0 ? (
            <Empty description={t('post.content.empty', 'No posts have been published yet.')} />
          ) : (
            posts?.map((post) =>
              isMobile ? (
                <MobilePostCard key={post.id} post={post} onTogglePinned={onTogglePinned} />
              ) : (
                <PostCard key={post.id} post={post} onTogglePinned={onTogglePinned} />
              )
            )
          )}
        </Stack>

        <nav aria-label={t('post.content.pagination_label', 'News result pages')} className={classes.paginationNav}>
          <Pagination.Root total={pageCount} siblings={isMobile ? 0 : 2} value={activePage} onChange={setPage} mb="xl">
            <Group gap={5} justify={isMobile ? 'center' : 'flex-end'}>
              {!isMobile && <Pagination.First aria-label={t('common.pagination.first', 'First page')} />}
              <Pagination.Previous aria-label={t('common.pagination.previous', 'Previous page')} />
              <Pagination.Items />
              <Pagination.Next aria-label={t('common.pagination.next', 'Next page')} />
              {!isMobile && <Pagination.Last aria-label={t('common.pagination.last', 'Last page')} />}
            </Group>
          </Pagination.Root>
        </nav>
      </Stack>
      {RequireRole(Role.Admin, role) && (
        <Button
          component={Link}
          className={misc.fixedButton}
          __vars={{
            '--fixed-right': '2rem',
            '--fixed-bottom': '6rem',
          }}
          variant="filled"
          size="md"
          leftSection={<Icon path={mdiPlus} size={1} />}
          to="/posts/new/edit"
        >
          {t('post.button.new')}
        </Button>
      )}
    </WithNavBar>
  )
}

export default Posts
