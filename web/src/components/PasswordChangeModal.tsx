import { Button, Group, Modal, ModalProps, PasswordInput, Stack } from '@mantine/core'
import { useInputState } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router'
import { StrengthPasswordInput } from '@Components/StrengthPasswordInput'
import { encryptApiData } from '@Utils/Crypto'
import { httpErrorStatus } from '@Utils/HttpError'
import { showErrorMsg } from '@Utils/Shared'
import { useConfig } from '@Hooks/useConfig'
import api from '@Api'

export const PasswordChangeModal: FC<ModalProps> = (props) => {
  const [oldPwd, setOldPwd] = useInputState('')
  const [pwd, setPwd] = useInputState('')
  const [retypedPwd, setRetypedPwd] = useInputState('')
  const [pending, setPending] = useState(false)
  const inFlight = useRef(false)

  const navigate = useNavigate()

  const { t } = useTranslation()
  const { config } = useConfig()

  const onChangePwd = async () => {
    if (inFlight.current) return
    if (!pwd || !retypedPwd) {
      showNotification({
        color: 'red',
        title: t('account.password.empty'),
        message: t('common.error.check_input'),
        icon: <Icon path={mdiClose} size={1} />,
      })
    } else if (pwd === retypedPwd) {
      inFlight.current = true
      setPending(true)
      try {
        await api.account.accountChangePassword({
          old: await encryptApiData(t, oldPwd, config.apiPublicKey),
          new: await encryptApiData(t, pwd, config.apiPublicKey),
        })
        showNotification({
          color: 'teal',
          message: t('account.notification.profile.password_updated'),
          icon: <Icon path={mdiCheck} size={1} />,
        })
        await api.account.accountLogOut()
        props.onClose()
        navigate('/account/login')
      } catch (e) {
        const status = httpErrorStatus(e)
        const ambiguous = status === null || status === 408 || status === 425 || (status !== null && status >= 500)
        if (ambiguous) {
          showNotification({
            color: 'orange',
            title: t('account.notification.profile.password_reconcile_title', 'Password status needs confirmation'),
            message: t(
              'account.notification.profile.password_reconcile_message',
              'The response was interrupted. Sign in with the intended new password before attempting another change.'
            ),
          })
          try {
            await api.account.accountLogOut()
          } catch {
            // The mutation result is ambiguous; leave this flow instead of
            // issuing another expensive password change from the modal.
          }
          props.onClose()
          navigate('/account/login')
        } else {
          showErrorMsg(e, t)
        }
      } finally {
        inFlight.current = false
        setPending(false)
      }
    } else {
      showNotification({
        color: 'red',
        title: t('account.password.not_match'),
        message: t('common.error.check_input'),
        icon: <Icon path={mdiClose} size={1} />,
      })
    }
  }

  return (
    <Modal
      {...props}
      onClose={() => {
        if (!inFlight.current) props.onClose()
      }}
      closeOnClickOutside={!pending}
      closeOnEscape={!pending}
      withCloseButton={!pending}
    >
      <Stack>
        <PasswordInput
          required
          label={t('account.label.password_old')}
          placeholder="P4ssW@rd"
          w="100%"
          value={oldPwd}
          disabled={pending}
          onChange={setOldPwd}
        />
        <StrengthPasswordInput value={pwd} onChange={setPwd} disabled={pending} />
        <PasswordInput
          required
          label={t('account.label.password_retype')}
          placeholder="P4ssW@rd"
          w="100%"
          value={retypedPwd}
          disabled={pending}
          onChange={setRetypedPwd}
        />

        <Group justify="right">
          <Button
            variant="default"
            disabled={pending}
            onClick={() => {
              setOldPwd('')
              setPwd('')
              setRetypedPwd('')
              props.onClose()
            }}
          >
            {t('common.modal.cancel')}
          </Button>
          <Button color="orange" onClick={onChangePwd} loading={pending} disabled={pending}>
            {t('common.modal.confirm_update')}
          </Button>
        </Group>
      </Stack>
    </Modal>
  )
}
