import { Button, Group, Modal, ModalProps, Stack, Switch, Text, Textarea } from '@mantine/core'
import { DateTimePicker } from '@mantine/dates'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { showErrorMsg } from '@Utils/Shared'
import api, { GameNotice } from '@Api'

interface GameNoticeEditModalProps extends ModalProps {
  gameNotice?: GameNotice | null
  mutateGameNotice: (gameNotice: GameNotice) => void
}

export const GameNoticeEditModal: FC<GameNoticeEditModalProps> = (props) => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')
  const { gameNotice, mutateGameNotice, ...modalProps } = props

  const [content, setContent] = useState<string>(gameNotice?.values.at(-1) || '')
  const [scheduled, setScheduled] = useState(false)
  const [publishAt, setPublishAt] = useState<Date | null>(null)
  const [disabled, setDisabled] = useState(false)
  const submittingRef = useRef(false)
  const operationRef = useRef<{ fingerprint: string; operationId: string } | null>(null)
  const generationRef = useRef(0)
  const { t } = useTranslation()
  const contentBytes = new TextEncoder().encode(content).length
  const maxContentBytes = 48 * 1024

  useEffect(() => {
    generationRef.current += 1
    submittingRef.current = false
    operationRef.current = null
    setDisabled(false)
    setContent(gameNotice?.values.at(-1) || '')
    setScheduled(false)
    setPublishAt(null)
    // Pre-populate schedule if existing notice has a future publish time
    if (gameNotice?.time) {
      const t = new Date(gameNotice.time)
      if (t > new Date()) {
        setScheduled(true)
        setPublishAt(t)
      }
    }
  }, [gameNotice])

  const onConfirm = async () => {
    if (submittingRef.current) return
    if (!content) {
      showNotification({
        color: 'red',
        message: t('common.error.empty'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }
    if (contentBytes > maxContentBytes) {
      showNotification({
        color: 'red',
        message: t('admin.notification.games.notices.too_large', {
          defaultValue: 'Notice content must be at most {{bytes}} UTF-8 bytes.',
          bytes: maxContentBytes,
        }),
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }
    if (content === gameNotice?.values.at(-1) && !scheduled) {
      showNotification({
        color: 'orange',
        message: t('common.error.no_change'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }

    submittingRef.current = true
    setDisabled(true)
    const generation = generationRef.current
    const fingerprint = JSON.stringify([
      gameNotice?.id ?? null,
      content,
      scheduled,
      scheduled && publishAt ? publishAt.toISOString() : null,
    ])
    if (operationRef.current?.fingerprint !== fingerprint) {
      operationRef.current = { fingerprint, operationId: crypto.randomUUID() }
    }
    const operation = operationRef.current!
    let succeeded = false

    try {
      const body = {
        content: content.trim(),
        operationId: operation.operationId,
        publishAt: scheduled && publishAt ? publishAt.getTime() : null,
      }
      const res = gameNotice
        ? await api.edit.editUpdateGameNotice(numId, gameNotice.id, body)
        : await api.edit.editAddGameNotice(numId, body)
      showNotification({
        color: 'teal',
        message:
          scheduled && publishAt
            ? t('admin.notification.games.notices.scheduled')
            : t(`admin.notification.games.notices.${gameNotice ? 'updated' : 'created'}`),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      mutateGameNotice(res.data)
      succeeded = true
      if (generationRef.current === generation) modalProps.onClose()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      if (generationRef.current === generation) {
        submittingRef.current = false
        setDisabled(false)
        if (succeeded) {
          operationRef.current = null
          setContent('')
          setScheduled(false)
          setPublishAt(null)
        }
      }
    }
  }

  return (
    <Modal
      {...modalProps}
      closeOnClickOutside={!disabled}
      closeOnEscape={!disabled}
      withCloseButton={!disabled}
      onClose={() => {
        if (!submittingRef.current) modalProps.onClose()
      }}
    >
      <Stack>
        <Text>{t('admin.content.markdown_inline_support')}</Text>
        <Textarea
          label={t('admin.label.games.notices.content', 'Notice content')}
          value={content}
          w="100%"
          autosize
          minRows={5}
          maxRows={16}
          maxLength={maxContentBytes}
          disabled={disabled}
          onChange={(e) => setContent(e.currentTarget.value)}
        />
        <Text size="xs" c={contentBytes > maxContentBytes ? 'red' : 'dimmed'} aria-live="polite">
          {contentBytes.toLocaleString()} / {maxContentBytes.toLocaleString()} UTF-8 bytes
        </Text>
        <Switch
          label={t('admin.label.games.notices.schedule')}
          checked={scheduled}
          disabled={disabled}
          onChange={(e) => {
            setScheduled(e.currentTarget.checked)
            if (!e.currentTarget.checked) setPublishAt(null)
          }}
        />
        {scheduled && (
          <DateTimePicker
            label={t('admin.label.games.notices.publish_at')}
            placeholder={t('admin.placeholder.games.notices.publish_at')}
            value={publishAt}
            onChange={(e) => setPublishAt(e ? new Date(e) : null)}
            minDate={new Date()}
            clearable
            disabled={disabled}
          />
        )}
        <Group grow m="auto" w="100%">
          <Button
            fullWidth
            disabled={disabled || contentBytes > maxContentBytes || (scheduled && !publishAt)}
            loading={disabled}
            onClick={onConfirm}
          >
            {t('common.modal.confirm')}
          </Button>
        </Group>
      </Stack>
    </Modal>
  )
}
