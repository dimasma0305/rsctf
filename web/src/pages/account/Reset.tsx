import { Button, PasswordInput, Stack } from '@mantine/core'
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
  const inFlight = useRef(false)
  const operationId = useRef(crypto.randomUUID())

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
      await api.account.accountPasswordReset({
        operationId: operationId.current,
        rToken: token,
        email: email,
        password: await encryptApiData(t, pwd, config.apiPublicKey),
      })
      showNotification({
        color: 'teal',
        title: t('account.notification.reset.success.title'),
        message: t('account.notification.reset.success.message'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      navigate('/account/login')
    } catch (e) {
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
      onSubmit={onReset}
    >
      <Stack w="100%">
        <StrengthPasswordInput
          value={pwd}
          onChange={(event) => {
            operationId.current = crypto.randomUUID()
            setPwd(event.currentTarget.value)
          }}
          label={t('account.label.password')}
          disabled={disabled}
        />
        <PasswordInput
          required
          value={retypedPwd}
          onChange={(event) => {
            operationId.current = crypto.randomUUID()
            setRetypedPwd(event.currentTarget.value)
          }}
          label={t('account.label.password_retype')}
          w="100%"
          disabled={disabled}
          error={pwd !== retypedPwd}
        />
        <Button fullWidth type="submit" loading={disabled} disabled={disabled}>
          {t('account.button.reset')}
        </Button>
      </Stack>
    </AccountView>
  )
}

export default Reset
