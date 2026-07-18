import { Avatar, Button, Group, Stack, Text, Title, useMantineTheme } from '@mantine/core'
import { useScrollIntoView } from '@mantine/hooks'
import { mdiPencilOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useEffect } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useNavigate, useParams } from 'react-router'
import { Markdown } from '@Components/MarkdownRenderer'
import { WithNavBar } from '@Components/WithNavbar'
import { RequireRole } from '@Components/WithRole'
import { useLanguage } from '@Utils/I18n'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useUserRole } from '@Hooks/useUser'
import api, { Role } from '@Api'
import misc from '@Styles/Misc.module.css'
import classes from '@Styles/PostDetail.module.css'

const Post: FC = () => {
  const { postId } = useParams()
  const navigate = useNavigate()

  const theme = useMantineTheme()
  const { t } = useTranslation()

  useEffect(() => {
    if (postId?.length !== 8) {
      navigate('/404')
      return
    }
  }, [postId, navigate])

  const { data: post } = api.info.useInfoGetPost(
    postId ?? '',
    {
      refreshInterval: 0,
      revalidateOnFocus: false,
    },
    postId?.length === 8
  )

  const { scrollIntoView, targetRef } = useScrollIntoView<HTMLDivElement>()
  useEffect(() => scrollIntoView({ alignment: 'center' }), [scrollIntoView])

  const { role } = useUserRole()
  const { locale } = useLanguage()
  const authorName = post?.authorName ?? t('post.content.anonymous_author', 'Anonymous')
  const publishedAt = dayjs(post?.time)

  usePageTitle(post?.title ?? 'Post')

  return (
    <WithNavBar width="1120px" isLoading={!post} withFooter>
      <article className={classes.article}>
        <header ref={targetRef} className={classes.header}>
          <Text className={classes.eyebrow}>{t('post.content.news', 'News & updates')}</Text>
          <Title order={1} className={classes.title}>
            {post?.title}
          </Title>

          <Group gap="sm" wrap="nowrap" className={classes.byline}>
            <Avatar
              alt={t('post.content.author_avatar', '{{author}} avatar', { author: authorName })}
              src={post?.authorAvatar}
              color={theme.primaryColor}
              radius="xl"
              size="md"
            >
              {authorName.slice(0, 1)}
            </Avatar>
            <Stack gap={1} className={classes.bylineCopy}>
              <Text fw={700} className={classes.author}>
                {authorName}
              </Text>
              <Text
                component="time"
                dateTime={publishedAt.isValid() ? publishedAt.toISOString() : undefined}
                size="sm"
                c="dimmed"
                className={classes.date}
              >
                {publishedAt.locale(locale).format('LLL')}
              </Text>
            </Stack>
          </Group>
        </header>

        <div className={classes.content}>
          <Markdown source={post?.content ?? ''} />
        </div>

        {post?.tags && post.tags.length > 0 && (
          <footer className={classes.footer} aria-label={t('post.content.tags', 'Post tags')}>
            <Group gap="xs" wrap="wrap">
              {post.tags.map((tag) => (
                <Text key={tag} fw={700} span className={classes.tag}>
                  {`#${tag}`}
                </Text>
              ))}
            </Group>
          </footer>
        )}
      </article>
      {RequireRole(Role.Admin, role) && (
        <Button
          component={Link}
          className={misc.fixedButton}
          variant="filled"
          radius="xl"
          size="md"
          leftSection={<Icon path={mdiPencilOutline} size={1} />}
          to={`/posts/${postId}/edit`}
        >
          {t('post.button.edit')}
        </Button>
      )}
    </WithNavBar>
  )
}

export default Post
