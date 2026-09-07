import { ActionIcon, Anchor, Avatar, Badge, Card, Group, Stack, Text, Title } from '@mantine/core'
import { mdiArrowRight, mdiPencilOutline, mdiPinOffOutline, mdiPinOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { Markdown } from '@Components/MarkdownRenderer'
import { RequireRole } from '@Components/WithRole'
import { useLanguage } from '@Utils/I18n'
import { useUserRole } from '@Hooks/useUser'
import { PostInfoModel, Role } from '@Api'
import classes from '@Styles/PostCard.module.css'

export interface PostCardProps {
  post: PostInfoModel
  headingOrder?: 2 | 3
  onTogglePinned?: (post: PostInfoModel, setDisabled: (value: boolean) => void) => void
}

export const PostCard: FC<PostCardProps> = ({ post, onTogglePinned, headingOrder = 2 }) => {
  const { role } = useUserRole()
  const { t } = useTranslation()
  const [disabled, setDisabled] = useState(false)

  const { locale } = useLanguage()
  const author = post.authorName ?? t('common.content.anonymous', 'Anonymous')
  const published = dayjs(post.time).locale(locale)

  return (
    <Card
      component="article"
      p={0}
      className={classes.card}
      data-post-card={post.id}
      data-pinned={post.isPinned || undefined}
    >
      <span className={classes.accent} aria-hidden="true" />
      <Stack gap="md" p={{ base: 'md', sm: 'lg' }} className={classes.content}>
        <Group justify="space-between" align="flex-start" wrap="nowrap" gap="md">
          <Group gap="sm" wrap="nowrap" align="flex-start" className={classes.headingGroup}>
            <Stack gap={6} className={classes.headingCopy}>
              <Group gap="sm" mb={4}>
                {post.isPinned && (
                  <Badge
                    variant="light"
                    size="sm"
                    className={classes.pinnedBadge}
                    leftSection={<Icon path={mdiPinOutline} size={0.6} aria-hidden="true" />}
                  >
                    {t('post.content.pinned_label', 'Pinned')}
                  </Badge>
                )}
                <Text
                  component="time"
                  dateTime={published.isValid() ? published.toISOString() : undefined}
                  title={published.isValid() ? published.format('LLL') : undefined}
                  className={classes.date}
                >
                  {published.isValid()
                    ? published.format('LL')
                    : t('post.content.date_unavailable', 'Publication date unavailable')}
                </Text>
              </Group>
              <Title order={headingOrder} className={classes.title}>
                <Link to={`/posts/${post.id}`} className={classes.titleLink}>
                  {post.title}
                </Link>
              </Title>
            </Stack>
          </Group>

          {RequireRole(Role.Admin, role) && (
            <Group gap={4} wrap="nowrap" className={classes.adminActions}>
              {onTogglePinned && (
                <ActionIcon
                  disabled={disabled}
                  aria-label={post.isPinned ? t('post.button.unpin', 'Unpin post') : t('post.button.pin', 'Pin post')}
                  onClick={() => onTogglePinned(post, setDisabled)}
                >
                  {post.isPinned ? (
                    <Icon path={mdiPinOffOutline} size={0.9} />
                  ) : (
                    <Icon path={mdiPinOutline} size={0.9} />
                  )}
                </ActionIcon>
              )}
              <ActionIcon
                component={Link}
                to={`/posts/${post.id}/edit`}
                aria-label={t('post.button.edit', 'Edit post')}
              >
                <Icon path={mdiPencilOutline} size={0.9} />
              </ActionIcon>
            </Group>
          )}
        </Group>

        {post.summary.trim() && (
          <div className={classes.summary}>
            <Markdown source={post.summary} />
          </div>
        )}

        {!!post.tags?.length && (
          <Group gap={6} className={classes.tags}>
            {post.tags.map((tag, idx) => (
              <Badge key={`${tag}-${idx}`} variant="light" color="gray" size="sm" className={classes.tag}>
                {`#${tag}`}
              </Badge>
            ))}
          </Group>
        )}

        <Group justify="space-between" align="center" wrap="wrap" gap="sm" className={classes.footer}>
          <Group gap="xs" wrap="nowrap" miw={0} className={classes.author}>
            <Avatar imageProps={{ loading: 'lazy' }} alt="" src={post.authorAvatar} size={28} aria-hidden="true">
              {post.authorName?.slice(0, 1) ?? 'A'}
            </Avatar>
            <Text size="sm" fw={600} className={classes.authorName}>
              {author}
            </Text>
          </Group>
          <Anchor
            component={Link}
            to={`/posts/${post.id}`}
            fw={700}
            size="sm"
            className={classes.details}
            aria-label={`${t('post.content.read_post', 'Read post')} — ${post.title}`}
          >
            {t('post.content.read_post', 'Read post')}
            <Icon path={mdiArrowRight} size={0.72} aria-hidden="true" />
          </Anchor>
        </Group>
      </Stack>
    </Card>
  )
}
