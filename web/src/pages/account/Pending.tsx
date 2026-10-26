import { Anchor, List, Stack, Text } from '@mantine/core'
import { FC } from 'react'
import { Trans, useTranslation } from 'react-i18next'
import { Link, useLocation, useSearchParams } from 'react-router'
import { AccountView } from '@Components/AccountView'
import { accountPath } from '@Utils/AccountNavigation'
import { usePageTitle } from '@Hooks/usePageTitle'
import misc from '@Styles/Misc.module.css'

const EmailConfirmationPending: FC = () => {
  const location = useLocation()
  const [params] = useSearchParams()
  const email = typeof location.state?.email === 'string' ? location.state.email.trim() : ''

  const { t } = useTranslation()
  usePageTitle(t('account.title.verify_email'))

  return (
    <AccountView title={t('account.content.verify_email.title')} description={t('account.title.verify_email')}>
      <Stack gap="xs" align="stretch" justify="center">
        <Text size="md" fw="bold" ta="center">
          {email ? (
            <Trans i18nKey="account.content.verify_email.message" />
          ) : (
            t('account.content.verify_email.missing_context')
          )}
        </Text>
        {email && (
          <Text size="md" fw="bold" ff="monospace" c="brand" ta="center" style={{ overflowWrap: 'anywhere' }}>
            {email}
          </Text>
        )}
        <Stack gap={4} mt="sm" align="stretch" w="100%">
          <Text size="xs" fw="bold" ta="center">
            {t('account.content.verify_email.not_received.title')}
          </Text>
          <List spacing={4} size="xs" c="dimmed" withPadding>
            <Trans i18nKey="account.content.verify_email.not_received.list">
              <List.Item />
              <List.Item />
            </Trans>
          </List>
        </Stack>
        <Anchor component={Link} to={accountPath('/account/register?resend=1', params.get('from'))}>
          {t('account.registration.resend_title')}
        </Anchor>
        <Anchor
          fz="xs"
          className={misc.alignSelfEnd}
          component={Link}
          to={accountPath('/account/login', params.get('from'))}
          mt="sm"
        >
          {t('account.anchor.login')}
        </Anchor>
      </Stack>
    </AccountView>
  )
}

export default EmailConfirmationPending
