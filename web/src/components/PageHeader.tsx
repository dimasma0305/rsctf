import { Group, Stack, Text, Title } from '@mantine/core'
import { FC, ReactNode } from 'react'
import classes from '@Styles/PageHeader.module.css'

interface PageHeaderProps {
  title: ReactNode
  description?: ReactNode
  eyebrow?: ReactNode
  actions?: ReactNode
}

export const PageHeader: FC<PageHeaderProps> = ({ title, description, eyebrow, actions }) => (
  <Group component="header" justify="space-between" align="flex-end" gap="lg" wrap="wrap" className={classes.root}>
    <Stack gap={4} className={classes.copy}>
      {eyebrow && <Text className={classes.eyebrow}>{eyebrow}</Text>}
      <Title order={1} className={classes.title}>
        {title}
      </Title>
      {description && (
        <Text size="sm" c="dimmed" className={classes.description}>
          {description}
        </Text>
      )}
    </Stack>
    {actions && <Group className={classes.actions}>{actions}</Group>}
  </Group>
)
