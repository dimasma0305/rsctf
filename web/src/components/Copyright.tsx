import { Anchor, Text } from '@mantine/core'
import { FC } from 'react'
import { RSCTF_REPOSITORY } from '@Hooks/useConfig'

interface CopyrightProps {
  isMobile?: boolean
}

export const Copyright: FC<CopyrightProps> = ({ isMobile }) => {
  const currentYear = new Date().getFullYear()

  return (
    <Text size="sm" ta="center" fw={400} c="dimmed">
      Copyright&nbsp;©&nbsp;{currentYear}&nbsp;
      <Anchor
        href={RSCTF_REPOSITORY}
        target="_blank"
        rel="noreferrer"
        c="dimmed"
        size="sm"
        fw={500}
        display="inline-flex"
        mih={44}
        style={{ alignItems: 'center' }}
      >
        RSCTF contributors
      </Anchor>
      {isMobile ? <br /> : <>&nbsp;·&nbsp;</>}
      <Anchor
        href="/legal/NOTICE"
        target="_blank"
        rel="noreferrer"
        c="dimmed"
        size="sm"
        fw={500}
        display="inline-flex"
        mih={44}
        style={{ alignItems: 'center' }}
      >
        Legal notices
      </Anchor>
    </Text>
  )
}
