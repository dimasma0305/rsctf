import { Box, Group, Text } from '@mantine/core'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { useIsMobile } from '@Utils/ThemeOverride'
import { useConfig } from '@Hooks/useConfig'
import classes from '@Styles/IconHeader.module.css'

interface StickyHeaderProps {
  sticky?: boolean
  px?: string
}

export const IconHeader: FC<StickyHeaderProps> = ({ sticky, px }) => {
  const { config } = useConfig()
  const { t } = useTranslation()
  const isMobile = useIsMobile()

  return isMobile ? (
    <Box h={8} />
  ) : (
    <Group
      __vars={{ '--header-px': px || undefined }}
      data-sticky={sticky || undefined}
      className={classes.group}
      aria-label={t('common.content.workspace_context', 'Workspace context')}
    >
      <Group gap="xs" wrap="nowrap">
        <span className={classes.statusDot} aria-hidden="true" />
        <Text size="xs" fw={750} tt="uppercase" className={classes.workspace}>
          {t('common.content.competition_workspace', 'Competition workspace')}
        </Text>
      </Group>
      <Text size="sm" className={classes.subtitle}>
        {config?.slogan?.trim() || 'Capture. Compete. Conquer.'}
      </Text>
    </Group>
  )
}
