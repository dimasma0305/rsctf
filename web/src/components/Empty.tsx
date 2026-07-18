import { MantineSize, Stack, Text } from '@mantine/core'
import { mdiInbox } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import classes from '@Styles/Empty.module.css'

interface EmptyProps {
  bordered?: boolean
  title?: ReactNode
  description?: ReactNode
  action?: ReactNode
  fontSize?: string | MantineSize | undefined
  mdiPath?: string
  iconSize?: number
}

export const Empty: FC<EmptyProps> = (props) => {
  const { t } = useTranslation()

  return (
    <Stack
      align="center"
      role="status"
      aria-live="polite"
      data-border={props.bordered || undefined}
      className={classes.box}
    >
      <span className={classes.icon} aria-hidden="true">
        <Icon path={props.mdiPath ?? mdiInbox} size={props.iconSize ?? 2.6} />
      </span>
      {props.title && (
        <Text fw={720} size="lg" className={classes.title}>
          {props.title}
        </Text>
      )}
      <Text c="dimmed" size={props.fontSize} ta="center" className={classes.description}>
        {props.description ?? t('common.content.no_data')}
      </Text>
      {props.action && <div className={classes.action}>{props.action}</div>}
    </Stack>
  )
}
