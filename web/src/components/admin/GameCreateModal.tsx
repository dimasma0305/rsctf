import { Button, Group, Modal, ModalProps, Stack, TextInput } from '@mantine/core'
import { DateTimePicker } from '@mantine/dates'
import { useInputState } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router'
import { RetryableMutationOwner } from '@Utils/RetryableMutationOwner'
import { showErrorMsg } from '@Utils/Shared'
import api, { GameInfoModel } from '@Api'

interface GameCreateModalProps extends ModalProps {
  onAddGame: (game: GameInfoModel) => void
}

export const GameCreateModal: FC<GameCreateModalProps> = (props) => {
  const { onAddGame, onClose, ...modalProps } = props
  const [disabled, setDisabled] = useState(false)
  const owner = useRef(new RetryableMutationOwner())
  const navigate = useNavigate()
  const [title, setTitle] = useInputState('')
  const [start, setStart] = useInputState(dayjs())
  const [end, setEnd] = useInputState(dayjs().add(2, 'h'))

  const { t } = useTranslation()

  useEffect(
    () => () => {
      owner.current.cancel()
    },
    []
  )

  const onCreate = async () => {
    const digest = JSON.stringify({ title: title.trim(), start: start.valueOf(), end: end.valueOf() })
    const lease = owner.current.claim(digest)
    if (!lease) return
    setDisabled(true)
    if (!title || end < start) {
      showNotification({
        color: 'red',
        title: t('common.error.param_invalid'),
        message: t('admin.notification.games.no_title_time'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      owner.current.settle(lease, true)
      setDisabled(false)
      return
    }

    try {
      const res = await api.edit.editAddGame(
        {
          title,
          start: start.valueOf(),
          end: end.valueOf(),
        },
        lease.operationId,
        { signal: lease.signal }
      )
      if (!owner.current.settle(lease, true)) return
      showNotification({
        color: 'teal',
        message: t('admin.notification.games.created'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      onAddGame(res.data)
      navigate(`/admin/games/${res.data.id}/info`)
    } catch (e) {
      if (!owner.current.settle(lease, false)) return
      showErrorMsg(e, t)
      setDisabled(false)
    }
  }

  const handleClose = () => {
    if (owner.current.isActive()) return
    owner.current.cancel()
    onClose()
  }

  return (
    <Modal
      size="min(36rem, calc(100vw - 2rem))"
      title={t('admin.button.games.new')}
      {...modalProps}
      onClose={handleClose}
      closeOnClickOutside={!disabled}
      closeOnEscape={!disabled}
      withCloseButton={!disabled}
    >
      <Stack
        component="form"
        onSubmit={(event) => {
          event.preventDefault()
          void onCreate()
        }}
      >
        <TextInput
          label={t('admin.content.games.info.title.label')}
          type="text"
          required
          disabled={disabled}
          w="100%"
          value={title}
          onChange={setTitle}
        />
        <DateTimePicker
          label={t('admin.content.games.info.start_time')}
          size="sm"
          value={start.toDate()}
          valueFormat="L LT"
          clearable={false}
          onChange={(e) => {
            const newDate = dayjs(e)
            setStart(newDate)
            if (newDate && end < newDate) {
              setEnd(newDate.add(2, 'h'))
            }
          }}
          required
          disabled={disabled}
        />
        <DateTimePicker
          label={t('admin.content.games.info.end_time')}
          size="sm"
          minDate={start.toDate()}
          valueFormat="L LT"
          value={end.toDate()}
          clearable={false}
          onChange={(e) => {
            setEnd(dayjs(e))
          }}
          error={end < start}
          required
          disabled={disabled}
        />
        <Group grow m="auto" w="100%">
          <Button type="submit" fullWidth disabled={disabled}>
            {t('admin.button.games.new')}
          </Button>
        </Group>
      </Stack>
    </Modal>
  )
}
