import { Alert, Button, PasswordInput, Stack, Text } from '@mantine/core'
import { useInputState } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, type FormEvent, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, useNavigate } from 'react-router'
import { AccountView } from '@Components/AccountView'
import { StrengthPasswordInput } from '@Components/StrengthPasswordInput'
import { encryptApiData } from '@Utils/Crypto'
import { httpErrorStatus, isRetryableHttpError } from '@Utils/HttpError'
import {
  clearPasswordResetOperation,
  passwordResetRequestSignature,
  PasswordResetOperation,
  retainPasswordResetOperation,
} from '@Utils/PasswordResetOperations'
import { showErrorMsg } from '@Utils/Shared'
import { useConfig } from '@Hooks/useConfig'
import { usePageTitle } from '@Hooks/usePageTitle'
import api from '@Api'

const Reset: FC = () => {
  const location = useLocation()
  const sp = new URLSearchParams(location.search)
  const token = sp.get('token')
  const email = sp.get('email')
  const navigate = useNavigate()
  const [pwd, setPwd] = useInputState('')
  const [retypedPwd, setRetypedPwd] = useInputState('')
  const [disabled, setDisabled] = useState(false)
  const [intentLocked, setIntentLocked] = useState(false)
  const [requiresNewLink, setRequiresNewLink] = useState(false)
  const inFlight = useRef(false)
  const operation = useRef<PasswordResetOperation | null>(null)

  const { t } = useTranslation()
  const { config } = useConfig()

  usePageTitle(t('account.title.reset'))

  const onReset = async (event?: FormEvent) => {
    event?.preventDefault()
    if (inFlight.current) return
    if (pwd !== retypedPwd) {
      showNotification({
        color: 'red',
        title: t('common.error.check_input'),
        message: t('account.password.not_match'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }

    if (!(token && email)) {
      showNotification({
        color: 'red',
        message: t('common.error.param_error'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }

    inFlight.current = true
    setDisabled(true)

    try {
      const signature = await passwordResetRequestSignature(token, email)
      const owner = retainPasswordResetOperation(sessionStorage, signature, operation.current)
      operation.current = owner
      await api.account.accountPasswordReset({
        operationId: owner.operationId,
        rToken: token,
        email: email,
        password: await encryptApiData(t, pwd, config.apiPublicKey),
      })
      clearPasswordResetOperation(sessionStorage, owner.operationId)
      operation.current = null
      setIntentLocked(false)
      showNotification({
        color: 'teal',
        title: t('account.notification.reset.success.title'),
        message: t('account.notification.reset.success.message'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      navigate('/account/login')
    } catch (e) {
      const status = httpErrorStatus(e)
      if (isRetryableHttpError(e)) {
        // An ambiguous response may already have committed this exact password.
        // Keep the inputs immutable and retry only the retained operation.
        setIntentLocked(true)
      } else if (status === 409) {
        // The token is already bound to a different exact password intent (most
        // commonly after reloading and typing a different value). Starting a
        // second operation would be rejected and could enqueue needless work.
        setIntentLocked(true)
        setRequiresNewLink(true)
      } else if (operation.current) {
        clearPasswordResetOperation(sessionStorage, operation.current.operationId)
        operation.current = null
        setIntentLocked(false)
        setRequiresNewLink(false)
      }
      showErrorMsg(e, t)
    } finally {
      inFlight.current = false
      setDisabled(false)
    }
  }

  return (
    <AccountView
      title={t('account.title.reset')}
      description={t('account.content.reset.description', 'Choose a strong new password for your account.')}
    >
      <Stack component="form" w="100%" onSubmit={onReset}>
        <StrengthPasswordInput
          value={pwd}
          onChange={setPwd}
          label={t('account.label.password')}
          disabled={disabled || intentLocked}
        />
        <PasswordInput
          required
          value={retypedPwd}
          onChange={setRetypedPwd}
          label={t('account.label.password_retype')}
          w="100%"
          disabled={disabled || intentLocked}
          error={pwd !== retypedPwd}
        />
        {intentLocked && !requiresNewLink && (
          <Alert color="orange">
            <Text size="sm">
              {t(
                'account.content.reset.retry_exact',
                'The previous response was interrupted. Your password is locked so this button retries only the same reset attempt.'
              )}
            </Text>
          </Alert>
        )}
        {requiresNewLink && (
          <Alert color="red">
            <Text size="sm">
              {t(
                'account.content.reset.new_link_required',
                'This link is already bound to another password attempt. Request a new reset link to choose a different password.'
              )}
            </Text>
            <Button type="button" mt="sm" variant="light" onClick={() => navigate('/account/recovery')}>
              {t('account.button.recovery', 'Request new reset link')}
            </Button>
          </Alert>
        )}
        <Button fullWidth type="submit" loading={disabled} disabled={disabled || requiresNewLink}>
          {t('account.button.reset')}
        </Button>
      </Stack>
    </AccountView>
  )
}

export default Reset
