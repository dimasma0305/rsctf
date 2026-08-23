import { Stack } from '@mantine/core'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { Navigate } from 'react-router'
import DonationPanel from '@Components/DonationPanel'
import { PageHeader } from '@Components/PageHeader'
import { WithNavBar } from '@Components/WithNavbar'
import { useConfig } from '@Hooks/useConfig'
import { usePageTitle } from '@Hooks/usePageTitle'

const Donations: FC = () => {
  const { t } = useTranslation()
  const { config, loading } = useConfig()

  usePageTitle(t('common.tab.donations', 'Donations'))

  if (loading) return <WithNavBar isLoading />
  if (!config.donationsEnabled) return <Navigate to="/" replace />

  return (
    <WithNavBar withFooter>
      <Stack gap="lg">
        <PageHeader
          eyebrow={t('common.content.donations.eyebrow', 'Community support')}
          title={t('common.tab.donations', 'Donations')}
          description={t(
            'common.content.donations.description',
            'Celebrate everyone who supports the platform and read their public messages.'
          )}
        />
        <DonationPanel donateUrl={config.donationUrl} />
      </Stack>
    </WithNavBar>
  )
}

export default Donations
