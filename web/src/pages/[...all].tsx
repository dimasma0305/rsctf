import { Stack, Text, Title } from '@mantine/core'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { WithNavBar } from '@Components/WithNavbar'
import { Icon404 } from '@Components/icon/404Icon'
import { usePageTitle } from '@Hooks/usePageTitle'
import classes from '@Styles/Placeholder.module.css'

// Render the 404 in place — deliberately NOT rewriting the URL to /404. The old
// navigate('/404') destroyed the address the user actually hit (so they couldn't
// see/share what 404'd) and broke the back button (it would land back on /404).
const Error404: FC = () => {
  const { t } = useTranslation()

  usePageTitle(t('common.title.404'))

  return (
    <WithNavBar>
      <Stack gap={0} className={classes.board}>
        <Icon404 />
        <Title order={1}>{t('common.content.404.title')}</Title>
        <Text fw="bold">{t('common.content.404.text')}</Text>
      </Stack>
    </WithNavBar>
  )
}

export default Error404
