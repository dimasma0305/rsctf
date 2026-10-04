import { Anchor, Button, TextInput } from '@mantine/core'
import { useInputState } from '@mantine/hooks'
import { showNotification, updateNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { AccountView } from '@Components/AccountView'
import { Captcha, useCaptchaRef } from '@Components/Captcha'
import { beginMailOperation, finishMailOperation, type MailOperationOwner } from '@Utils/MailOperation'
import { tryGetErrorMsg } from '@Utils/Shared'
import { usePageTitle } from '@Hooks/usePageTitle'
import api from '@Api'
import misc from '@Styles/Misc.module.css'

const Recovery: FC = () => {
  const [email, setEmail] = useInputState('')
  const [disabled, setDisabled] = useState(false)
  const recoveryOperationRef = useRef<MailOperationOwner | null>(null)
  const { captchaRef, getToken, cleanUp } = useCaptchaRef()

  const { t } = useTranslation()

  usePageTitle(t('account.title.recovery'))
  useEffect(() => () => recoveryOperationRef.current?.controller.abort(), [])

  const onRecovery = async (event: React.SyntheticEvent) => {
    event.preventDefault()
    const acquired = beginMailOperation(recoveryOperationRef.current, email.trim().toLowerCase())
    if (!acquired.started) return
    const operation = acquired.owner
    recoveryOperationRef.current = operation
    setDisabled(true)
    let completed = false

    try {
      const { valid, token } = await getToken()

      if (!valid) {
        showNotification({
          color: 'orange',
          title: t('account.notification.captcha.not_valid'),
          message: t('common.error.try_later'),
          loading: true,
        })
        return
      }

      showNotification({
        color: 'orange',
        id: 'recovery-status',
        title: t('account.notification.captcha.request_sent.title'),
        message: t('account.notification.captcha.request_sent.message'),
        loading: true,
        autoClose: false,
      })

      await api.account.accountRecovery(
        {
          email,
          challenge: token,
          operationId: operation.operationId,
        },
        { signal: operation.controller.signal }
      )
      completed = true

      updateNotification({
        id: 'recovery-status',
        color: 'teal',
        title: t('common.email.sent.title'),
        message: t('common.email.sent.message'),
        icon: <Icon path={mdiCheck} size={1} />,
        loading: false,
        autoClose: true,
      })
      cleanUp(true)
    } catch (err: any) {
      if (operation.controller.signal.aborted) return
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
      if (recoveryOperationRef.current === operation)
        recoveryOperationRef.current = finishMailOperation(operation, completed)
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
      <Button type="submit" disabled={disabled} fullWidth>
        {t('account.button.recovery')}
      </Button>
    </AccountView>
  )
}

export default Recovery
