import {
  Button,
  ComboboxItem,
  ModalProps,
  NumberInput,
  Select,
  Stack,
  Switch,
  Textarea,
  TextInput,
} from '@mantine/core'
import { useInputState } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import { mdiCheck } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate, useParams } from 'react-router'
import { AccessibleModal } from '@Components/AccessibleModal'
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
  returnToSetup?: boolean
}

export const ChallengeCreateModal: FC<ChallengeCreateModalProps> = (props) => {
  const { id } = useParams()
  const { onAddChallenge, onClose, opened, returnToSetup, ...modalProps } = props
  const [disabled, setDisabled] = useState(false)
  const navigate = useNavigate()
  const challengeCategoryLabelMap = useChallengeCategoryLabelMap()
  const challengeTypeLabelMap = useChallengeTypeLabelMap()

  const [title, setTitle] = useInputState('')
  const [category, setCategory] = useState<string | null>(ChallengeCategory.Misc)
  const [type, setType] = useState<string | null>(ChallengeType.StaticAttachment)
  const [advanced, setAdvanced] = useState(false)
  const [content, setContent] = useInputState('')
  const [answer, setAnswer] = useInputState('')
  const [points, setPoints] = useState<string | number>(100)
  const createOperation = useRef<ChallengeMutationOperation | null>(null)
  const requestOwner = useRef(new RetryableMutationOwner())

  const { t } = useTranslation()

  useEffect(() => {
    requestOwner.current.cancel()
    createOperation.current = null
    setDisabled(false)
    if (!opened) {
      setTitle('')
      setContent('')
      setAnswer('')
      setPoints(100)
      setAdvanced(false)
      setCategory(ChallengeCategory.Misc)
      setType(ChallengeType.StaticAttachment)
    }
    return () => requestOwner.current.cancel()
  }, [id, opened])

  const onCreate = async () => {
    if (!title.trim() || !category || !type || (!advanced && (!content.trim() || !answer || !Number.isInteger(points))))
      return

    const numId = parseInt(id ?? '-1')
    const prepared = prepareChallengeMutation(
      {
        title,
        category: advanced ? (category as ChallengeCategory) : ChallengeCategory.Misc,
        type: advanced ? (type as ChallengeType) : ChallengeType.StaticAttachment,
        ...(!advanced ? { content, initialFlag: answer, originalScore: Number(points) } : {}),
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
      onClose()
      navigate(returnToSetup ? `/admin/games/${id}/setup` : `/admin/games/${id}/challenges/${res.data.id}`)
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
    setCategory(ChallengeCategory.Misc)
    setType(ChallengeType.StaticAttachment)
    setContent('')
    setAnswer('')
    setPoints(100)
    setAdvanced(false)
    onClose()
  }

  return (
    <AccessibleModal
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
        {!advanced && (
          <>
            <Textarea
              required
              autosize
              minRows={3}
              label={t('admin.easy.instructions', 'Instructions for players')}
              value={content}
              onChange={setContent}
              disabled={disabled}
              maxLength={65536}
            />
            <TextInput
              required
              label={t('admin.easy.answer', 'Answer (flag)')}
              description={t('admin.easy.answer_hint', 'Players must submit this exact answer. Keep it secret.')}
              value={answer}
              onChange={setAnswer}
              disabled={disabled}
              autoComplete="off"
            />
            <NumberInput
              required
              label={t('admin.easy.points', 'Points')}
              min={1}
              max={100000}
              allowDecimal={false}
              value={points}
              onChange={setPoints}
              disabled={disabled}
            />
          </>
        )}
        <Switch
          label={t('admin.easy.advanced', 'Advanced options')}
          checked={advanced}
          disabled={disabled}
          onChange={(event) => setAdvanced(event.currentTarget.checked)}
        />
        {advanced && (
          <>
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
          </>
        )}
        <Button type="submit" fullWidth disabled={disabled || !title || !category || !type}>
          {t('admin.button.challenges.new')}
        </Button>
      </Stack>
    </AccessibleModal>
  )
}
