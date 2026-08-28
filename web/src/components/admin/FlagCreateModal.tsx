import { Button, Group, Modal, ModalProps, Stack, Text, Textarea } from '@mantine/core'
import { useInputState } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import { mdiCheck } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useRef, useState } from 'react'
import { Trans, useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { showErrorMsg } from '@Utils/Shared'
import { useEditChallenge } from '@Hooks/useEdit'
import api from '@Api'
import misc from '@Styles/Misc.module.css'
import { MAX_FLAG_IMPORT_ROWS, parsePlainFlagRows, validateFlagRows } from '@Utils/FlagImport'

export const FlagCreateModal: FC<ModalProps> = (props) => {
  const [disabled, setDisabled] = useState(false)
  const submitting = useRef(false)
  const operationId = useRef<string | null>(null)

  const { id, chalId } = useParams()
  const [numId, numCId] = [parseInt(id ?? '-1'), parseInt(chalId ?? '-1')]
  const [flags, setFlags] = useInputState('')

  const { challenge, mutate } = useEditChallenge(numId, numCId)

  const { t } = useTranslation()

  const onCreate = async () => {
    if (!flags) {
      return
    }

    if (submitting.current) return
    const flagList = parsePlainFlagRows(flags)
    const validationError = validateFlagRows(flagList)
    if (validationError) {
      showNotification({ color: 'red', message: validationError })
      return
    }

    submitting.current = true
    setDisabled(true)

    try {
      operationId.current ??= crypto.randomUUID()
      const response = await api.edit.editAddFlags(numId, numCId, {
        operationId: operationId.current,
        flags: flagList,
      })
      showNotification({
        color: 'teal',
        message: `${response.data.inserted} added; ${response.data.duplicates} duplicate(s) skipped.`,
        icon: <Icon path={mdiCheck} size={1} />,
      })
      if (challenge) await mutate()
      setFlags('')
      operationId.current = null
      props.onClose()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      submitting.current = false
      setDisabled(false)
    }
  }

  return (
    <Modal {...props}>
      <Stack>
        <Text size="sm">
          <Trans i18nKey="admin.content.games.challenges.flag.create" />
        </Text>
        <Text size="xs" c="dimmed">Up to {MAX_FLAG_IMPORT_ROWS} flags per import; one flag per line.</Text>
        <Textarea
          label={t('admin.label.games.challenges.flags', 'Flags')}
          w="100%"
          value={flags}
          disabled={disabled}
          autosize
          minRows={8}
          maxRows={8}
          onChange={(event) => {
            operationId.current = null
            setFlags(event)
          }}
          classNames={{
            input: misc.ffmono,
          }}
        />
        <Group grow m="auto" w="100%">
          <Button fullWidth disabled={disabled} onClick={onCreate}>
            {t('admin.button.challenges.flag.add.normal')}
          </Button>
        </Group>
      </Stack>
    </Modal>
  )
}
