import { Button, Modal, ModalProps, Stack, Text, Textarea } from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiCheck } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { showErrorMsg } from '@Utils/Shared'
import { useEditChallenge } from '@Hooks/useEdit'
import api from '@Api'
import misc from '@Styles/Misc.module.css'
import { MAX_FLAG_IMPORT_ROWS, parseRemoteFlagRows, validateFlagRows } from '@Utils/FlagImport'

export const AttachmentRemoteEditModal: FC<ModalProps> = (props) => {
  const { id, chalId } = useParams()
  const [numId, numCId] = [parseInt(id ?? '-1'), parseInt(chalId ?? '-1')]

  const [disabled, setDisabled] = useState(false)
  const submitting = useRef(false)
  const operationId = useRef<string | null>(null)

  const { mutate } = useEditChallenge(numId, numCId)

  const [text, setText] = useState('')
  const flags = useMemo(() => parseRemoteFlagRows(text), [text])

  const { t } = useTranslation()

  const onUpload = async () => {
    if (submitting.current) return
    const validationError = validateFlagRows(flags)
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
        flags,
      })
      showNotification({
        color: 'teal',
        message: `${response.data.inserted} added; ${response.data.duplicates} duplicate(s) skipped.`,
        icon: <Icon path={mdiCheck} size={1} />,
      })
      setText('')
      operationId.current = null
      mutate()
      props.onClose()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      submitting.current = false
      setDisabled(false)
    }
  }

  return (
    <Modal {...props} closeOnClickOutside={!disabled} closeOnEscape={!disabled}>
      <Stack>
        <Text>
          {t('admin.content.games.challenges.attachment.instruction.remote.content')}
          <br />
          <Text fw="bold" span>
            {t('admin.content.games.challenges.attachment.instruction.remote.format')}
          </Text>
          <br />
          <Text fw="bold" c="orange" span>
            {t('admin.content.games.challenges.attachment.instruction.amount_double')}
          </Text>
          <br />
        </Text>
        <Text size="xs" c="dimmed">Up to {MAX_FLAG_IMPORT_ROWS} flag and URL pairs per import.</Text>
        <Textarea
          label={t('admin.label.games.challenges.attachment_pairs', 'Flag and attachment URL pairs')}
          required
          autosize
          minRows={8}
          maxRows={12}
          value={text}
          disabled={disabled}
          classNames={{ input: misc.ffmono }}
          onChange={(e) => {
            operationId.current = null
            setText(e.target.value)
          }}
          placeholder={'flag{hello_world} http://example.com/1.zip\nflag{he11o_world} http://example.com/2.zip'}
        />
        <Button fullWidth loading={disabled} disabled={disabled} onClick={onUpload}>
          {t('admin.button.games.challenges.attachment.batch_add')}
        </Button>
      </Stack>
    </Modal>
  )
}
