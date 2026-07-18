import { Anchor, Box, Paper, Stack, Text, Title } from '@mantine/core'
import { mdiArrowLeft } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, PropsWithChildren, ReactNode, useId } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { LogoHeader } from '@Components/LogoHeader'
import { useConfig } from '@Hooks/useConfig'
import classes from '@Styles/AccountView.module.css'

interface AccountViewProps extends PropsWithChildren {
  title: ReactNode
  description?: ReactNode
  onSubmit?: (event: React.SubmitEvent<HTMLFormElement>) => Promise<void>
}

export const AccountView: FC<AccountViewProps> = ({ title, description, onSubmit, children }) => {
  const { config } = useConfig()
  const { t } = useTranslation()
  const titleId = useId()

  return (
    <main id="main-content" tabIndex={-1} className={classes.shell}>
      <Box component="aside" className={classes.context} aria-label={t('common.content.platform', 'Platform')}>
        <Link to="/" className={classes.brandLink}>
          <LogoHeader />
        </Link>
        <Stack gap="md" className={classes.contextCopy}>
          <Text className={classes.eyebrow}>{t('common.content.competition_workspace', 'Competition workspace')}</Text>
          <Text className={classes.statement}>{config?.slogan?.trim() || 'Capture. Compete. Conquer.'}</Text>
          <Text size="md" className={classes.contextDescription}>
            {t(
              'common.content.account_context',
              'Sign in to join your team, solve challenges, and follow the competition in real time.'
            )}
          </Text>
        </Stack>
        <Text size="xs" className={classes.contextFooter}>
          {config?.title?.trim() || 'RS::CTF'}
        </Text>
      </Box>

      <section className={classes.formPanel} aria-labelledby={titleId}>
        <Paper className={classes.card} p={{ base: 'lg', sm: 'xl' }}>
          <Anchor component={Link} to="/" className={classes.backLink}>
            <Icon path={mdiArrowLeft} size={0.72} aria-hidden="true" />
            {t('common.button.back_home', 'Back to home')}
          </Anchor>

          <div className={classes.mobileBrand}>
            <LogoHeader />
          </div>

          <Stack gap={5} mb="lg">
            <Title id={titleId} order={1} className={classes.title}>
              {title}
            </Title>
            <Text c="dimmed" size="sm">
              {description ?? (config?.slogan?.trim() || 'Capture. Compete. Conquer.')}
            </Text>
          </Stack>

          <form className={classes.form} onSubmit={onSubmit} aria-labelledby={titleId}>
            <Stack gap="sm">{children}</Stack>
          </form>
        </Paper>
      </section>
    </main>
  )
}
