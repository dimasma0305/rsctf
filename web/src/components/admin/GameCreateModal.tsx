import { Button, Group, Modal, ModalProps, Stack, TextInput } from '@mantine/core'
import { DateTimePicker } from '@mantine/dates'
import { useInputState } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router'
import { createIntentStorageKey, useDurableCreateIntent } from '@Utils/DurableCreateIntent'
import { showErrorMsg } from '@Utils/Shared'
import api, { GameInfoModel } from '@Api'

interface GameCreateModalProps extends ModalProps {
  onAddGame: (game: GameInfoModel) => void
}

export const GameCreateModal: FC<GameCreateModalProps> = (props) => {
  const { onAddGame, ...modalProps } = props
  const navigate = useNavigate()
  const [title, setTitle] = useInputState('')
  const [start, setStart] = useInputState(dayjs())
  const [end, setEnd] = useInputState(dayjs().add(2, 'h'))

  const { t } = useTranslation()

  const { busy: disabled, submit } = useDurableCreateIntent({
    storageKey: createIntentStorageKey('game'),
    enabled: modalProps.opened,
    request: (payload: Pick<GameInfoModel, 'title' | 'start' | 'end'>, operationId, signal) =>
      api.edit.editAddGame({ ...payload, operationId }, { signal }),
    onSuccess: (res) => {
      showNotification({
        color: 'teal',
        message: t('admin.notification.games.created'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      onAddGame(res.data)
      navigate(`/admin/games/${res.data.id}/info`)
    },
    onError: (error) => showErrorMsg(error, t),
  })

  const onCreate = async () => {
    if (disabled) return
    if (!title || end < start) {
      showNotification({
        color: 'red',
        title: t('common.error.param_invalid'),
        message: t('admin.notification.games.no_title_time'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }

    await submit({ title, start: start.valueOf(), end: end.valueOf() })
  }

  const handleClose = () => {
    if (disabled) return
    modalProps.onClose()
  }

  return (
    <Modal
      size="min(36rem, calc(100vw - 2rem))"
      title={t('admin.button.games.new')}
      {...modalProps}
      onClose={handleClose}
      closeOnClickOutside={!disabled}
      closeOnEscape={!disabled}
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
          disabled={disabled}
          onChange={(e) => {
            const newDate = dayjs(e)
            setStart(newDate)
            if (newDate && end < newDate) {
              setEnd(newDate.add(2, 'h'))
            }
          }}
          required
        />
        <DateTimePicker
          label={t('admin.content.games.info.end_time')}
          size="sm"
          minDate={start.toDate()}
          valueFormat="L LT"
          value={end.toDate()}
          clearable={false}
          disabled={disabled}
          onChange={(e) => {
            setEnd(dayjs(e))
          }}
          error={end < start}
          required
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
