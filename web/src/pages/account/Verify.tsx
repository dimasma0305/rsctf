import { Button, Text } from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, useNavigate } from 'react-router'
import { AccountView } from '@Components/AccountView'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useAccountLinkSubmit } from '@Hooks/useAccountLinkSubmit'
import api from '@Api'

const Verify: FC = () => {
  const navigate = useNavigate()
  const location = useLocation()
  const sp = new URLSearchParams(location.search)
  const token = sp.get('token')
  const email = sp.get('email')
  const { pending, run } = useAccountLinkSubmit(`${token ?? ''}\0${email ?? ''}`)
  let decodeEmail = ''
  try {
    decodeEmail = email ? window.atob(email) : ''
  } catch {
    decodeEmail = ''
  }

  const { t } = useTranslation()

  usePageTitle(t('account.title.verify'))

  const verify = async (event: React.SyntheticEvent) => {
    event.preventDefault()

    if (!token || !email) {
      showNotification({
        color: 'red',
        title: t('account.notification.verify.failed'),
        message: t('common.error.param_missing'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }

    await run(
      (signal) => api.account.accountVerify({ token, email }, { signal }),
      () => {
        showNotification({
          color: 'teal',
          title: t('account.notification.verify.success'),
          message: decodeEmail,
          icon: <Icon path={mdiCheck} size={1} />,
        })
        navigate('/account/login')
      },
      () =>
        showNotification({
          color: 'red',
          title: t('account.notification.verify.failed'),
          message: t('common.error.param_error'),
          icon: <Icon path={mdiClose} size={1} />,
        }),
    )
  }

  return (
    <AccountView title={t('account.title.verify')} onSubmit={verify}>
      {email && token ? (
        <>
          <Text size="md" fw={500}>
            {t('account.content.welcome', { decodeEmail })}
          </Text>
          <Text size="md" fw={500}>
            {t('account.content.verify.message')}
          </Text>
          <Button
            mt="lg"
            type="submit"
            w={{ base: '100%', xs: '50%' }}
            disabled={pending}
            loading={pending}
          >
            {t('account.button.verify_account')}
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

export default Verify
