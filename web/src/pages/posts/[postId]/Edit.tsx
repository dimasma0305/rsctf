import { Button, Group, SimpleGrid, Stack, TagsInput, Text, Textarea, TextInput, Title } from '@mantine/core'
import { useModals } from '@mantine/modals'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiContentSaveOutline, mdiDeleteOutline, mdiFileCheckOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate, useParams } from 'react-router'
import { useSWRConfig } from 'swr'
import { WithNavBar } from '@Components/WithNavbar'
import { WithRole } from '@Components/WithRole'
import { createIntentStorageKey, useDurableCreateIntent } from '@Utils/DurableCreateIntent'
import { invalidatePostPageCaches } from '@Utils/PostFeed'
import { showErrorMsg } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import api, { PostEditModel, Role } from '@Api'

const PostEdit: FC = () => {
  const { postId } = useParams()
  const navigate = useNavigate()
  const { mutate: mutateCache } = useSWRConfig()

  const { t } = useTranslation()

  useEffect(() => {
    if (postId?.length !== 8 && postId !== 'new') {
      navigate('/404')
      return
    }
  }, [postId, navigate])

  const { data: curPost } = api.info.useInfoGetPost(
    postId ?? '',
    {
      refreshInterval: 0,
      revalidateOnFocus: false,
      shouldRetryOnError: false,
    },
    postId?.length === 8
  )

  const [post, setPost] = useState<PostEditModel>({
    title: curPost?.title ?? '',
    content: curPost?.content ?? '',
    summary: curPost?.summary ?? '',
    tags: curPost?.tags ?? [],
  })

  const [tags, setTags] = useState<string[]>([])
  const [updateDisabled, setUpdateDisabled] = useState(false)
  const saveOwnerRef = useRef<AbortController | null>(null)
  const [hasChanged, setHasChanged] = useState(false)

  const modals = useModals()

  const isMobile = useIsMobile()

  useEffect(() => {
    saveOwnerRef.current?.abort()
    saveOwnerRef.current = null
    setUpdateDisabled(false)
    return () => {
      saveOwnerRef.current?.abort()
      saveOwnerRef.current = null
    }
  }, [postId])

  const { busy: createBusy, submit: submitCreate } = useDurableCreateIntent({
    storageKey: createIntentStorageKey('post'),
    enabled: postId === 'new',
    request: (payload: PostEditModel, operationId, signal) =>
      api.edit.editAddPost({ ...payload, operationId }, { signal }),
    onSuccess: async (res) => {
      await Promise.all([
        api.info.mutateInfoGetLatestPosts(),
        api.info.mutateInfoGetPosts(),
        invalidatePostPageCaches(mutateCache),
      ])
      showNotification({
        color: 'teal',
        message: t('post.notification.created'),
        icon: <Icon path={mdiCheck} size={24} />,
      })
      setHasChanged(false)
      navigate(`/posts/${res.data}/edit`)
    },
    onError: (error) => showErrorMsg(error, t),
  })
  const disabled = updateDisabled || createBusy

  const onUpdate = async (): Promise<boolean> => {
    if (postId === 'new') {
      if (createBusy) return false
      return submitCreate(post)
    } else if (postId?.length === 8) {
      if (saveOwnerRef.current) return false
      const owner = new AbortController()
      saveOwnerRef.current = owner
      setUpdateDisabled(true)

      try {
        // Temporary workaround for an issue where posts could not be updated.
        // Ideally, the pin/unpin functionality should be handled by a separate API endpoint.
        const { isPinned: _, ...postWithoutPin } = post

        const res = await api.edit.editUpdatePost(postId, postWithoutPin, { signal: owner.signal })
        if (saveOwnerRef.current !== owner) return false
        await Promise.all([
          api.info.mutateInfoGetPost(postId, res.data),
          api.info.mutateInfoGetLatestPosts(),
          api.info.mutateInfoGetPosts(),
          invalidatePostPageCaches(mutateCache),
        ])
        if (saveOwnerRef.current !== owner) return false
        showNotification({
          color: 'teal',
          message: t('post.notification.saved'),
          icon: <Icon path={mdiCheck} size={24} />,
        })
        setHasChanged(false)
        return true
      } catch (e) {
        if (saveOwnerRef.current === owner && !owner.signal.aborted) showErrorMsg(e, t)
        return false
      } finally {
        if (saveOwnerRef.current === owner) {
          saveOwnerRef.current = null
          setUpdateDisabled(false)
        }
      }
    }
    return false
  }

  const onDelete = async () => {
    if (!postId) return
    setUpdateDisabled(true)

    try {
      await api.edit.editDeletePost(postId)
      api.info.mutateInfoGetPosts()
      api.info.mutateInfoGetLatestPosts()
      void invalidatePostPageCaches(mutateCache)
      navigate('/posts')
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setUpdateDisabled(false)
    }
  }

  useEffect(() => {
    if (!curPost) return

    setPost({
      title: curPost.title,
      content: curPost.content,
      summary: curPost.summary,
      isPinned: curPost.isPinned,
      tags: curPost.tags ?? [],
    })
    setTags(curPost.tags ?? [])
  }, [curPost])

  useEffect(() => {
    if (!curPost) return
    setHasChanged(
      post.title !== curPost.title ||
        post.content !== curPost.content ||
        post.summary !== curPost.summary ||
        post.isPinned !== curPost.isPinned ||
        (post.tags?.some((tag) => !curPost?.tags?.includes(tag)) ?? false)
    )
  }, [post, curPost])

  const titlePart = (
    <>
      <TextInput
        disabled={disabled}
        label={t('post.label.title')}
        value={post.title ?? ''}
        onChange={(e) => setPost({ ...post, title: e.currentTarget.value })}
      />
      <TagsInput
        disabled={disabled}
        label={t('post.label.tag')}
        data={tags.map((o) => ({ value: o, label: o })) || []}
        placeholder={t('post.label.add_tag')}
        value={post?.tags ?? []}
        onChange={(values) => setPost({ ...post, tags: values })}
        styles={{ inputField: { minHeight: 28 } }}
        clearable
      />
    </>
  )

  return (
    <WithNavBar withHeader stickyHeader>
      <WithRole requiredRole={Role.Admin}>
        <Stack mt={isMobile ? 25 : 30}>
          <Group justify="space-between" align="center" wrap="wrap">
            <Title order={1} size="h2" c="dimmed">
              {`> ${postId === 'new' ? t('post.button.new') : t('post.button.edit')}`}
            </Title>
            <Group justify="right">
              {postId?.length === 8 && (
                <>
                  <Button
                    disabled={disabled}
                    color="red"
                    leftSection={<Icon path={mdiDeleteOutline} size={1} />}
                    variant="outline"
                    onClick={() =>
                      modals.openConfirmModal({
                        title: t('post.button.delete'),
                        children: (
                          <Text size="sm">
                            {t('post.content.delete', {
                              title: curPost?.title,
                            })}
                          </Text>
                        ),
                        onConfirm: onDelete,
                        confirmProps: { color: 'red' },
                      })
                    }
                  >
                    {t('post.button.delete')}
                  </Button>
                  <Button
                    disabled={disabled}
                    leftSection={<Icon path={mdiFileCheckOutline} size={1} />}
                    onClick={() => {
                      if (hasChanged) {
                        modals.openConfirmModal({
                          title: t('post.content.updated.title'),
                          children: <Text size="sm">{t('post.content.updated.content')}</Text>,
                          onConfirm: async () => {
                            if (await onUpdate()) navigate(`/posts/${postId}`)
                          },
                        })
                      } else {
                        navigate(`/posts/${postId}`)
                      }
                    }}
                  >
                    {t('post.button.goto')}
                  </Button>
                </>
              )}
              <Button
                disabled={disabled}
                leftSection={<Icon path={mdiContentSaveOutline} size={1} />}
                onClick={onUpdate}
              >
                {postId === 'new' ? t('post.button.new') : t('post.button.save')}
              </Button>
            </Group>
          </Group>
          {isMobile ? titlePart : <SimpleGrid cols={2}>{titlePart}</SimpleGrid>}
          <Textarea
            disabled={disabled}
            label={
              <Group gap="sm">
                <Text size="sm">{t('post.label.summary')}</Text>
                <Text size="xs" c="dimmed">
                  {t('admin.content.markdown_support')}
                </Text>
              </Group>
            }
            autosize
            value={post.summary ?? ''}
            onChange={(e) => setPost({ ...post, summary: e.currentTarget.value })}
            minRows={5}
            maxRows={5}
          />
          <Textarea
            disabled={disabled}
            label={
              <Group gap="sm">
                <Text size="sm">{t('post.label.content')}</Text>
                <Text size="xs" c="dimmed">
                  {t('admin.content.markdown_support')}
                </Text>
              </Group>
            }
            autosize
            value={post.content ?? ''}
            onChange={(e) => setPost({ ...post, content: e.currentTarget.value })}
            minRows={isMobile ? 14 : 16}
            maxRows={isMobile ? 14 : 16}
          />
        </Stack>
      </WithRole>
    </WithNavBar>
  )
}

export default PostEdit
