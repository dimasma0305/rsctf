import { Alert, Button, Text } from '@mantine/core'
import { mdiAlertCircleOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import { useTranslation } from 'react-i18next'

/** Failed reads remain distinguishable from successfully loaded empty data. */
export function RequestError(props: {
  name: string
  stale?: boolean
  retrying?: boolean
  onRetry: () => Promise<unknown>
}) {
  const { t } = useTranslation()
  return (
    <Alert
      role="alert"
      color="red"
      data-request-error
      title={t('interface.request.failed', { name: props.name })}
      icon={<Icon path={mdiAlertCircleOutline} size={0.9} aria-hidden="true" />}
    >
      <Text size="sm">{t(props.stale ? 'interface.request.stale' : 'interface.request.hint')}</Text>
      <Button
        mt="sm"
        variant="outline"
        loading={props.retrying}
        onClick={() => void props.onRetry().catch(() => undefined)}
      >
        {t('interface.actions.retry')}
      </Button>
    </Alert>
  )
}
