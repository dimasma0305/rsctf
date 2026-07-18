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
    <Center component="main" id="main-content" tabIndex={-1} mih="100dvh" p="md" className={classes.shell}>
      <Paper
        p={{ base: 'lg', sm: 'xl' }}
        maw="60rem"
        miw={isMobile ? 'auto' : '30rem'}
        w="100%"
        className={classes.card}
      >
        <Stack gap="md">
          <Title fw="bold" order={1} c={theme.primaryColor}>
            {t('common.error.encountered')}
          </Title>
          <Text fz="lg" fw={500} role="alert">
            {getErrorMessage(error)}
          </Text>
          <Textarea
            label={t('common.content.diagnostic_details', 'Diagnostic details')}
            value={getErrorStack(error)}
            readOnly
            autosize
            minRows={12}
            maxRows={20}
            tabIndex={-1}
            styles={{
              input: {
                fontFamily: theme.fontFamilyMonospace,
                fontSize: theme.fontSizes.sm,
              },
            }}
          />
          <Text ta="center" size="sm" fw="bold" c="dimmed">
            &gt;&gt;&gt; {t('common.content.report_error')}&lt;&lt;&lt;
          </Text>
          <Group grow>
            <Button variant="outline" onClick={resetErrorBoundary}>
              {t('common.button.try_again')}
            </Button>
            <Button variant="outline" onClick={clearLocalCache}>
              {t('common.tab.account.clean_cache')}
            </Button>
          </Group>
        </Stack>
      </Paper>
    </Center>
  )
}
