import { Text } from '@mantine/core'
import { mdiArrowTopRight } from '@mdi/js'
import { Icon } from '@mdi/react'
import { Link } from 'react-router'
import classes from '@Styles/WorkspaceLinks.module.css'

interface WorkspaceLink {
  title: string
  description: string
  to: string
  icon: string
}

export function WorkspaceLinks({ label, items }: { label: string; items: WorkspaceLink[] }) {
  return (
    <nav className={classes.grid} aria-label={label}>
      {items.map((item) => (
        <Link key={item.to} to={item.to} className={classes.link}>
          <Icon className={classes.icon} path={item.icon} size={1} aria-hidden="true" />
          <div className={classes.copy}>
            <Text fw={600} size="sm">
              {item.title}
            </Text>
            <Text size="xs" c="dimmed">
              {item.description}
            </Text>
          </div>
          <Icon className={classes.arrow} path={mdiArrowTopRight} size={0.8} aria-hidden="true" />
        </Link>
      ))}
    </nav>
  )
}
