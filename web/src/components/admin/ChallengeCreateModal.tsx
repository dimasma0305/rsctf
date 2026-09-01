import { Button, ComboboxItem, Modal, ModalProps, Select, Stack, TextInput } from '@mantine/core'
import { useInputState } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import { mdiCheck } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate, useParams } from 'react-router'
import { ChallengeMutationOperation, prepareChallengeMutation } from '@Utils/ChallengeMutation'
import { RetryableMutationOwner } from '@Utils/RetryableMutationOwner'
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
  const { onAddChallenge, onClose, opened, ...modalProps } = props
  const [disabled, setDisabled] = useState(false)
  const navigate = useNavigate()
  const challengeCategoryLabelMap = useChallengeCategoryLabelMap()
  const challengeTypeLabelMap = useChallengeTypeLabelMap()

  const [title, setTitle] = useInputState('')
  const [category, setCategory] = useState<string | null>(null)
  const [type, setType] = useState<string | null>(null)
  const createOperation = useRef<ChallengeMutationOperation | null>(null)
  const requestOwner = useRef(new RetryableMutationOwner())

  const { t } = useTranslation()

  useEffect(() => {
    requestOwner.current.cancel()
    createOperation.current = null
    setDisabled(false)
    return () => requestOwner.current.cancel()
  }, [id, opened])

  const onCreate = async () => {
    if (!title || !category || !type) return

    const numId = parseInt(id ?? '-1')
    const prepared = prepareChallengeMutation(
      {
        title,
        category: category as ChallengeCategory,
        type: type as ChallengeType,
      },
      undefined,
      createOperation.current
    )
    const lease = requestOwner.current.claim(prepared.operation.digest, prepared.operation.id)
    if (!lease) return
    createOperation.current = prepared.operation
    setDisabled(true)

    try {
      const res = await api.edit.editAddGameChallenge(numId, prepared.payload, { signal: lease.signal })
      if (!requestOwner.current.settle(lease, true)) return
      createOperation.current = null
      showNotification({
        color: 'teal',
        message: t('admin.notification.games.challenges.created'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      onAddChallenge(res.data)
      navigate(`/admin/games/${id}/challenges/${res.data.id}`)
    } catch (e) {
      if (!requestOwner.current.settle(lease, false)) return
      showErrorMsg(e, t)
      setDisabled(false)
    }
  }

  const handleClose = () => {
    if (requestOwner.current.isActive()) return
    requestOwner.current.cancel()
    createOperation.current = null
    setTitle('')
    setCategory(null)
    setType(null)
    onClose()
  }

  return (
    <Modal
      {...modalProps}
      opened={opened}
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
          label={t('admin.content.games.challenges.title')}
          type="text"
          required
          placeholder="Title"
          value={title}
          disabled={disabled}
          onChange={setTitle}
        />
        <Select
          required
          label={t('admin.content.games.challenges.category')}
          placeholder="Category"
          value={category}
          disabled={disabled}
          onChange={setCategory}
          renderOption={ChallengeCategoryItem}
          data={ChallengeCategoryList.map((category) => {
            const data = challengeCategoryLabelMap.get(category)
            return { value: category, label: data?.name, ...data } as ComboboxItem
          })}
        />
        <Select
          required
          label={t('admin.content.games.challenges.type.label')}
          description={t('admin.content.games.challenges.type.description')}
          placeholder="Type"
          value={type}
          disabled={disabled}
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
