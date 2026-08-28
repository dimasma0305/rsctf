import { Button, ComboboxItem, Modal, ModalProps, Select, Stack, TextInput } from '@mantine/core'
import { useInputState } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import { mdiCheck } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate, useParams } from 'react-router'
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
  const [disabled, setDisabled] = useState(false)
  const createOwnerRef = useRef<AbortController | null>(null)
  const operationIdRef = useRef<string | null>(null)
  const navigate = useNavigate()
  const challengeCategoryLabelMap = useChallengeCategoryLabelMap()
  const challengeTypeLabelMap = useChallengeTypeLabelMap()

  const [title, setTitle] = useInputState('')
  const [category, setCategory] = useState<string | null>(null)
  const [type, setType] = useState<string | null>(null)

  const { t } = useTranslation()

  useEffect(() => {
    createOwnerRef.current?.abort()
    createOwnerRef.current = null
    operationIdRef.current = null
    setDisabled(false)
    return () => {
      createOwnerRef.current?.abort()
      createOwnerRef.current = null
    }
  }, [id])

  useEffect(() => {
    if (!createOwnerRef.current) operationIdRef.current = null
  }, [title, category, type])

  const onCreate = async () => {
    if (createOwnerRef.current || !title || !category || !type) return

    const owner = new AbortController()
    createOwnerRef.current = owner
    setDisabled(true)
    const numId = parseInt(id ?? '-1')
    const operationId = operationIdRef.current ?? crypto.randomUUID()
    operationIdRef.current = operationId

    try {
      const res = await api.edit.editAddGameChallenge(
        numId,
        {
          operationId,
          title: title,
          category: category as ChallengeCategory,
          type: type as ChallengeType,
        },
        { signal: owner.signal }
      )
      if (createOwnerRef.current !== owner) return
      showNotification({
        color: 'teal',
        message: t('admin.notification.games.challenges.created'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      operationIdRef.current = null
      onAddChallenge(res.data)
      navigate(`/admin/games/${id}/challenges/${res.data.id}`)
    } catch (e) {
      if (createOwnerRef.current === owner && !owner.signal.aborted) showErrorMsg(e, t)
    } finally {
      if (createOwnerRef.current === owner) {
        createOwnerRef.current = null
        setDisabled(false)
      }
    }
  }

  const handleClose = () => {
    if (createOwnerRef.current) return
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
