import { ActionIcon, Anchor, Avatar, Badge, Card, Group, Stack, Text, ThemeIcon, Title, Tooltip } from '@mantine/core'
import { mdiArrowRight, mdiFormatQuoteOpen, mdiPencilOutline, mdiPinOffOutline, mdiPinOutline } from '@mdi/js'
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
  onTogglePinned?: (post: PostInfoModel, setDisabled: (value: boolean) => void) => void
}

export const PostCard: FC<PostCardProps> = ({ post, onTogglePinned }) => {
  const { role } = useUserRole()
  const { t } = useTranslation()
  const [disabled, setDisabled] = useState(false)

  const { locale } = useLanguage()

  return (
    <Card component="article" p={0} className={classes.card}>
      <span className={classes.accent} aria-hidden="true" />
      <Stack gap="md" p={{ base: 'md', sm: 'lg' }} className={classes.content}>
        <Group justify="space-between" align="flex-start" wrap="nowrap" gap="md">
          <Group gap="sm" wrap="nowrap" align="flex-start" className={classes.headingGroup}>
            <ThemeIcon variant="light" radius="lg" size={44} className={classes.quote}>
              <Icon path={mdiFormatQuoteOpen} size={1.05} aria-hidden="true" />
            </ThemeIcon>
            <Stack gap={6} className={classes.headingCopy}>
              {post.isPinned && (
                <Badge variant="light" size="sm" className={classes.pinnedBadge}>
                  {t('post.content.pinned')}
                </Badge>
              )}
              <Title order={3} className={classes.title}>
                {post.title}
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

        <div className={classes.summary}>
          <Markdown source={post.summary} />
        </div>

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
            <Avatar imageProps={{ loading: 'lazy' }} alt={post.authorName ?? ''} src={post.authorAvatar} size={32}>
              {post.authorName?.slice(0, 1) ?? 'A'}
            </Avatar>
            <Tooltip label={post.authorName} disabled={!post.authorName}>
              <Text size="sm" fw={650} c="dimmed" truncate>
                {t('post.content.metadata', {
                  author: post.authorName ?? t('common.content.anonymous', 'Anonymous'),
                  date: dayjs(post.time).locale(locale).format('LLL'),
                })}
              </Text>
            </Tooltip>
          </Group>
          <Anchor
            component={Link}
            to={`/posts/${post.id}`}
            fw={700}
            size="sm"
            className={classes.details}
            aria-label={`${t('post.content.details')} — ${post.title}`}
          >
            {t('post.content.details')}
            <Icon path={mdiArrowRight} size={0.72} aria-hidden="true" />
          </Anchor>
        </Group>
      </Stack>
    </Card>
  )
}
