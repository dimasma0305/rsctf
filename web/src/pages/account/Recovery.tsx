import { Anchor, Button, TextInput } from '@mantine/core'
import { useInputState } from '@mantine/hooks'
import { showNotification, updateNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { AccountView } from '@Components/AccountView'
import { Captcha, useCaptchaRef } from '@Components/Captcha'
import { showErrorMsg, tryGetErrorMsg } from '@Utils/Shared'
import { usePageTitle } from '@Hooks/usePageTitle'
import api from '@Api'
import misc from '@Styles/Misc.module.css'

interface RecoveryOperationOwner {
  email: string
  operationId: string
}

const RECOVERY_OPERATION_KEY = 'rsctf:account-recovery-operation'

const retainRecoveryOperation = (email: string): RecoveryOperationOwner => {
  try {
    const stored = sessionStorage.getItem(RECOVERY_OPERATION_KEY)
    const candidate = stored ? (JSON.parse(stored) as Partial<RecoveryOperationOwner>) : null
    if (candidate?.email === email && typeof candidate.operationId === 'string') {
      return { email, operationId: candidate.operationId }
    }
  } catch {
    // Privacy-restricted browsers fall back to a fresh in-memory request.
  }
  const owner = { email, operationId: crypto.randomUUID() }
  try {
    sessionStorage.setItem(RECOVERY_OPERATION_KEY, JSON.stringify(owner))
  } catch {
    // The returned owner still protects this request lifetime.
  }
  return owner
}

const clearRecoveryOperation = (operationId: string) => {
  try {
    const stored = sessionStorage.getItem(RECOVERY_OPERATION_KEY)
    const candidate = stored ? (JSON.parse(stored) as Partial<RecoveryOperationOwner>) : null
    if (candidate?.operationId === operationId) sessionStorage.removeItem(RECOVERY_OPERATION_KEY)
  } catch {
    // The authoritative response already completed the intent.
  }
}

const Recovery: FC = () => {
  const [email, setEmail] = useInputState('')
  const [disabled, setDisabled] = useState(false)
  const inFlight = useRef(false)
  const { captchaRef, getToken, cleanUp } = useCaptchaRef()

  const { t } = useTranslation()

  usePageTitle(t('account.title.recovery'))

  const onRecovery = async (event: React.SyntheticEvent) => {
    event.preventDefault()
    if (inFlight.current) return
    inFlight.current = true

    let captcha
    try {
      captcha = await getToken()
    } catch (error) {
      inFlight.current = false
      showErrorMsg(error, t)
      return
    }
    const { valid, token } = captcha

    if (!valid) {
      inFlight.current = false
      showNotification({
        color: 'orange',
        title: t('account.notification.captcha.not_valid'),
        message: t('common.error.try_later'),
        loading: true,
      })
      return
    }

    setDisabled(true)
    const operation = retainRecoveryOperation(email.trim().toLowerCase())

    showNotification({
      color: 'orange',
      id: 'recovery-status',
      title: t('account.notification.captcha.request_sent.title'),
      message: t('account.notification.captcha.request_sent.message'),
      loading: true,
      autoClose: false,
    })

    try {
      await api.account.accountRecovery({
        email,
        challenge: token,
        operationId: operation.operationId,
      })

      updateNotification({
        id: 'recovery-status',
        color: 'teal',
        title: t('common.email.sent.title'),
        message: t('common.email.sent.message'),
        icon: <Icon path={mdiCheck} size={1} />,
        loading: false,
        autoClose: true,
      })
      clearRecoveryOperation(operation.operationId)
      cleanUp(true)
    } catch (err: any) {
      updateNotification({
        id: 'recovery-status',
        color: 'red',
        title: t('common.error.encountered'),
        message: tryGetErrorMsg(err, t),
        icon: <Icon path={mdiClose} size={1} />,
        loading: false,
        autoClose: true,
      })
      cleanUp(false)
    } finally {
      inFlight.current = false
      setDisabled(false)
    }
  }

  return (
    <AccountView
      title={t('account.title.recovery')}
      description={t('account.content.recovery.description', 'We will send password reset instructions to your email.')}
      onSubmit={onRecovery}
    >
      <TextInput
        required
        label={t('account.label.email')}
        placeholder="ctf@example.com"
        type="email"
        w="100%"
        value={email}
        disabled={disabled}
        onChange={(event) => setEmail(event.currentTarget.value)}
      />
      <Captcha action="recovery" ref={captchaRef} />
      <Anchor fz="xs" className={misc.alignSelfEnd} component={Link} to="/account/login">
        {t('account.anchor.login')}
      </Anchor>
      <Button disabled={disabled} loading={disabled} fullWidth type="submit">
        {t('account.button.recovery')}
      </Button>
    </AccountView>
  )
}

export default Recovery
