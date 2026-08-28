import { Button, ComboboxItem, Modal, ModalProps, Select, Stack, TextInput } from '@mantine/core'
import { useInputState } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import { mdiCheck } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate, useParams } from 'react-router'
import { createIntentStorageKey, useDurableCreateIntent } from '@Utils/DurableCreateIntent'
import { showErrorMsg } from '@Utils/Shared'
import {
  ChallengeCategoryItem,
  ChallengeCategoryList,
  ChallengeTypeItem,
  useChallengeCategoryLabelMap,
  useChallengeTypeLabelMap,
} from '@Utils/Shared'
import api, { ChallengeInfoModel, ChallengeCategory, ChallengeType } from '@Api'

interface ChallengeCreateModalProps extends ModalProps {
  onAddChallenge: (game: ChallengeInfoModel) => void
}

export const ChallengeCreateModal: FC<ChallengeCreateModalProps> = (props) => {
  const { id } = useParams()
  const { onAddChallenge, onClose, ...modalProps } = props
  const navigate = useNavigate()
  const challengeCategoryLabelMap = useChallengeCategoryLabelMap()
  const challengeTypeLabelMap = useChallengeTypeLabelMap()

  const [title, setTitle] = useInputState('')
  const [category, setCategory] = useState<string | null>(null)
  const [type, setType] = useState<string | null>(null)

  const { t } = useTranslation()

  const intentKey = createIntentStorageKey('challenge', id ?? 'invalid')
  const { busy: disabled, submit } = useDurableCreateIntent({
    storageKey: intentKey,
    enabled: Boolean(modalProps.opened && id),
    request: (payload: { title: string; category: ChallengeCategory; type: ChallengeType }, operationId, signal) =>
      api.edit.editAddGameChallenge(parseInt(id ?? '-1'), { ...payload, operationId }, { signal }),
    onSuccess: (res) => {
      showNotification({
        color: 'teal',
        message: t('admin.notification.games.challenges.created'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      onAddChallenge(res.data)
      navigate(`/admin/games/${id}/challenges/${res.data.id}`)
    },
    onError: (error) => showErrorMsg(error, t),
  })

  const onCreate = async () => {
    if (disabled || !title || !category || !type) return
    await submit({ title, category: category as ChallengeCategory, type: type as ChallengeType })
  }

  const handleClose = () => {
    if (disabled) return
    setTitle('')
    setCategory(null)
    setType(null)
    onClose()
  }

  return (
    <Modal {...modalProps} onClose={handleClose} closeOnClickOutside={!disabled} closeOnEscape={!disabled}>
      <Stack
        component="form"
        onSubmit={(event) => {
          event.preventDefault()
          void onCreate()
        }}
      >
        <TextInput
          label={t('admin.content.games.challenges.title')}
          type="text"
          required
          disabled={disabled}
          placeholder="Title"
          value={title}
          onChange={setTitle}
        />
        <Select
          required
          disabled={disabled}
          label={t('admin.content.games.challenges.category')}
          placeholder="Category"
          value={category}
          onChange={setCategory}
          renderOption={ChallengeCategoryItem}
          data={ChallengeCategoryList.map((category) => {
            const data = challengeCategoryLabelMap.get(category)
            return { value: category, label: data?.name, ...data } as ComboboxItem
          })}
        />
        <Select
          required
          disabled={disabled}
          label={t('admin.content.games.challenges.type.label')}
          description={t('admin.content.games.challenges.type.description')}
          placeholder="Type"
          value={type}
          onChange={setType}
          renderOption={ChallengeTypeItem}
          data={Object.entries(ChallengeType).map((type) => {
            const data = challengeTypeLabelMap.get(type[1])
            return { value: type[1], label: data?.name, ...data } as ComboboxItem
          })}
        />
        <Button type="submit" fullWidth disabled={disabled || !title || !category || !type}>
          {t('admin.button.challenges.new')}
        </Button>
      </Stack>
    </Modal>
  )
}
