import { Button, Group, Modal, ModalProps, NumberInput, Stack, Text } from '@mantine/core'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { BloodBonus } from '@Utils/Shared'
import { OnceSWRConfig } from '@Hooks/useConfig'
import api, { SubmissionType } from '@Api'
import {
  buildGameInfoUpdatePayload,
  type GameInfoSaveOperation,
  prepareGameInfoSave,
} from '../../pages/admin/games/[id]/gameInfoDraft'

const toNumber = (value: string | number) => {
  if (typeof value === 'string') {
    const val = Number(value)
    return isNaN(val) ? 0 : val
  }
  return value
}

export const BloodBonusModel: FC<ModalProps> = (props) => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')
  const { data: gameSource, mutate } = api.edit.useEditGetGame(numId, OnceSWRConfig)
  const [disabled, setDisabled] = useState(false)
  const [firstBloodBonus, setFirstBloodBonus] = useState(0)
  const [secondBloodBonus, setSecondBloodBonus] = useState(0)
  const [thirdBloodBonus, setThirdBloodBonus] = useState(0)
  const saveOperation = useRef<GameInfoSaveOperation | null>(null)

  const { t } = useTranslation()

  useEffect(() => {
    if (gameSource) {
      const bonus = new BloodBonus(gameSource.bloodBonus)
      setFirstBloodBonus(bonus.getBonusNum(SubmissionType.FirstBlood))
      setSecondBloodBonus(bonus.getBonusNum(SubmissionType.SecondBlood))
      setThirdBloodBonus(bonus.getBonusNum(SubmissionType.ThirdBlood))
    }
  }, [gameSource])

  const onUpdate = async () => {
    if (!gameSource?.title) return
    setDisabled(true)

    try {
      const payload = buildGameInfoUpdatePayload(
        {
          ...gameSource,
          bloodBonus: BloodBonus.fromBonus(firstBloodBonus, secondBloodBonus, thirdBloodBonus).value,
        },
        {
          start: gameSource.start,
          end: gameSource.end,
          freeze: gameSource.freeze ?? null,
          writeupDeadline: gameSource.writeupDeadline ?? gameSource.end,
        },
        false
      )
      const prepared = prepareGameInfoSave(payload, saveOperation.current)
      saveOperation.current = prepared.operation
      const response = await api.edit.editUpdateGame(numId, prepared.payload)
      saveOperation.current = null
      await mutate(response.data, { revalidate: false })
      props.onClose()
    } finally {
      setDisabled(false)
    }
  }

  return (
    <Modal {...props}>
      <Stack>
        <Text>{t('admin.content.games.challenges.bonus.description')}</Text>
        <NumberInput
          label={t('admin.content.games.challenges.bonus.first_blood')}
          defaultValue={5}
          decimalScale={1}
          fixedDecimalScale
          min={0}
          step={1}
          max={100}
          disabled={disabled}
          value={firstBloodBonus / 10}
          onChange={(value) => setFirstBloodBonus(Math.floor(toNumber(value) * 10))}
        />
        <NumberInput
          label={t('admin.content.games.challenges.bonus.second_blood')}
          defaultValue={3}
          decimalScale={1}
          fixedDecimalScale
          min={0}
          step={1}
          max={100}
          disabled={disabled}
          value={secondBloodBonus / 10}
          onChange={(value) => setSecondBloodBonus(Math.floor(toNumber(value) * 10))}
        />
        <NumberInput
          label={t('admin.content.games.challenges.bonus.third_blood')}
          defaultValue={1}
          decimalScale={1}
          fixedDecimalScale
          min={0}
          step={1}
          max={100}
          disabled={disabled}
          value={thirdBloodBonus / 10}
          onChange={(value) => setThirdBloodBonus(Math.floor(toNumber(value) * 10))}
        />
        <Group grow m="auto" w="100%">
          <Button fullWidth disabled={disabled} onClick={onUpdate}>
            {t('admin.button.save')}
          </Button>
        </Group>
      </Stack>
    </Modal>
  )
}
