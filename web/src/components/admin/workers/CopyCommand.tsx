import { Button, Code, CopyButton, Stack } from '@mantine/core'
import { mdiCheck, mdiContentCopy } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC } from 'react'
import classes from '@Styles/AdminWorkers.module.css'

interface CopyCommandProps {
  command: string
  label: string
  copiedLabel: string
  icon?: string
}

export const CopyCommand: FC<CopyCommandProps> = ({ command, label, copiedLabel, icon = mdiContentCopy }) => (
  <Stack gap="sm">
    <Code block className={classes.command}>
      {command}
    </Code>
    <CopyButton value={command} timeout={1800}>
      {({ copied, copy }) => (
        <Button
          variant="light"
          color={copied ? 'teal' : undefined}
          leftSection={<Icon path={copied ? mdiCheck : icon} size={0.8} aria-hidden="true" />}
          onClick={copy}
        >
          {copied ? copiedLabel : label}
        </Button>
      )}
    </CopyButton>
  </Stack>
)
