import { Button, Text } from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, useNavigate } from 'react-router'
import { AccountView } from '@Components/AccountView'
import { RetryableMutationOwner } from '@Utils/RetryableMutationOwner'
import { usePageTitle } from '@Hooks/usePageTitle'
import api from '@Api'

const Confirm: FC = () => {
  const navigate = useNavigate()
  const location = useLocation()
  const sp = new URLSearchParams(location.search)
  const token = sp.get('token')
  const email = sp.get('email')
  const [disabled, setDisabled] = useState(false)
  const owner = useRef(new RetryableMutationOwner())
  const { t } = useTranslation()
  // A corrupted/truncated link can carry a non-base64 email param; window.atob then
  // throws synchronously during render and white-screens the page. Decode safely and
  // let the !token/!email branch surface the "invalid link" message instead.
  let decodeEmail = ''
  try {
    decodeEmail = email ? window.atob(email) : ''
  } catch {
    decodeEmail = ''
  }

  usePageTitle(t('account.title.confirm'))

  useEffect(() => {
    owner.current.cancel()
    setDisabled(false)
    return () => owner.current.cancel()
  }, [token, email])

  const verify = async (event: React.SyntheticEvent) => {
    event.preventDefault()

    if (!token || !email) {
      showNotification({
        color: 'red',
        title: t('account.notification.confirm.failed'),
        message: t('common.error.param_missing'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }

    const lease = owner.current.claim(JSON.stringify({ token, email }))
    if (!lease) return
    setDisabled(true)

    try {
      await api.account.accountMailChangeConfirm({ token, email }, { signal: lease.signal })
      if (!owner.current.settle(lease, true)) return
      showNotification({
        color: 'teal',
        title: t('account.notification.confirm.success'),
        message: decodeEmail,
        icon: <Icon path={mdiCheck} size={1} />,
      })
      navigate('/')
    } catch {
      if (!owner.current.settle(lease, false)) return
      showNotification({
        color: 'red',
        title: t('account.notification.confirm.failed'),
        message: t('common.error.param_error'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      setDisabled(false)
    }
  }

  return (
    <AccountView title={t('account.title.confirm')} onSubmit={verify}>
      {email && token ? (
        <>
          <Text size="md" fw={500}>
            {t('account.content.welcome', { decodeEmail })}
          </Text>
          <Text size="md" fw={500}>
            {t('account.content.confirm.message')}
          </Text>
          <Button mt="lg" type="submit" w={{ base: '100%', xs: '50%' }} disabled={disabled} loading={disabled}>
            {t('account.button.confirm_email')}
          </Button>
        </>
      ) : (
        <>
          <Text size="md" fw={500}>
            {t('account.content.link_invalid')}
          </Text>
          <Text size="md" fw={500}>
            {t('account.content.link_check')}
          </Text>
        </>
      )}
    </AccountView>
  )
}

export default Confirm
