import { Button, Group, Pagination, Stack } from '@mantine/core'
import { mdiPlus } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { Empty } from '@Components/Empty'
import { PageHeader } from '@Components/PageHeader'
import { PostCard } from '@Components/PostCard'
import { WithNavBar } from '@Components/WithNavbar'
import { RequireRole } from '@Components/WithRole'
import { MobilePostCard } from '@Components/mobile/PostCard'
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
  const { data: posts, mutate } = api.info.useInfoGetPosts(OnceSWRConfig)

  const [activePage, setPage] = useState(1)
  const isMobile = useIsMobile()
  const { role } = useUserRole()

  const { t } = useTranslation()

  usePageTitle(t('post.title.index'))

  const onTogglePinned = async (post: PostInfoModel, setDisabled: (value: boolean) => void) => {
    setDisabled(true)

    try {
      const res = await api.edit.editUpdatePost(post.id, {
        isPinned: !post.isPinned,
      })
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
      api.info.mutateInfoGetLatestPosts()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  return (
    <WithNavBar isLoading={!posts} withHeader stickyHeader>
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
            posts
              ?.slice((activePage - 1) * ITEMS_PER_PAGE, activePage * ITEMS_PER_PAGE)
              .map((post) =>
                isMobile ? (
                  <MobilePostCard key={post.id} post={post} onTogglePinned={onTogglePinned} />
                ) : (
                  <PostCard key={post.id} post={post} onTogglePinned={onTogglePinned} />
                )
              )
          )}
        </Stack>

        <nav aria-label={t('post.content.pagination_label', 'News result pages')} className={classes.paginationNav}>
          <Pagination.Root
            total={Math.ceil((posts?.length ?? 0) / ITEMS_PER_PAGE)}
            siblings={isMobile ? 0 : 2}
            value={activePage}
            onChange={setPage}
            mb="xl"
          >
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
