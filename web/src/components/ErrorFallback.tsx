import { Button, Center, Text, Stack, Title, useMantineTheme, Textarea, Group, Paper } from '@mantine/core'
import { FC } from 'react'
import { getErrorMessage, FallbackProps } from 'react-error-boundary'
import { useTranslation } from 'react-i18next'
import { clearLocalCache } from '@Utils/Cache'
import { useIsMobile } from '@Utils/ThemeOverride'
import classes from '@Styles/ErrorFallback.module.css'

function getErrorStack(thrown: unknown): string | undefined {
  if (typeof thrown === 'object' && thrown !== null && 'stack' in thrown && typeof thrown.stack === 'string') {
    return thrown.stack
  }

  return getErrorMessage(thrown)
}

export const ErrorFallback: FC<FallbackProps> = ({ error, resetErrorBoundary }: FallbackProps) => {
  const theme = useMantineTheme()
  const { t } = useTranslation()
  const isMobile = useIsMobile()

  return (
    <Center
      component="main"
      id="main-content"
      data-error-fallback
      tabIndex={-1}
      mih="100dvh"
      p="md"
      className={classes.shell}
    >
      <Paper
        p={{ base: 'lg', sm: 'xl' }}
        maw="38rem"
        miw={isMobile ? 'auto' : '30rem'}
        w="100%"
        className={classes.card}
      >
        <Stack gap="md">
          <Title fw="bold" order={1} c={theme.primaryColor}>
            {t('common.error.encountered')}
          </Title>
          <Text role="alert" c="dimmed">
            {t(
              'common.content.page_error_help',
              'This page could not be displayed. Try again, or return to the event list.'
            )}
          </Text>
          <Group className={classes.actions}>
            <Button onClick={resetErrorBoundary}>{t('common.button.try_again')}</Button>
            <Button component="a" href="/games" variant="default">
              {t('common.button.browse_events', 'Browse events')}
            </Button>
          </Group>
          <details className={classes.details}>
            <summary>{t('common.content.diagnostic_details', 'Diagnostic details')}</summary>
            <Stack gap="sm" mt="sm">
              <Text size="sm" c="dimmed">
                {t(
                  'common.content.report_error_help',
                  'If this keeps happening, share these details with the organizer.'
                )}
              </Text>
              <Textarea
                label={t('common.content.error_message', 'Error message')}
                value={getErrorStack(error)}
                readOnly
                autosize
                minRows={4}
                maxRows={8}
                styles={{ input: { fontFamily: theme.fontFamilyMonospace, fontSize: theme.fontSizes.sm } }}
              />
              <Button variant="outline" onClick={clearLocalCache}>
                {t('common.tab.account.clean_cache')}
              </Button>
            </Stack>
          </details>
        </Stack>
      </Paper>
    </Center>
  )
}
