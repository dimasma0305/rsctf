import { Text } from '@mantine/core'
import { Icon } from '@mdi/react'
import { Link } from 'react-router'
import classes from '@Styles/WorkspaceLinks.module.css'

interface WorkspaceLink {
  title: string
  to: string
  icon: string
}

export function WorkspaceLinks({ label, items }: { label: string; items: WorkspaceLink[] }) {
  return (
    <nav className={classes.links} aria-label={label} data-workspace-links>
      {items.map((item) => (
        <Link key={item.to} to={item.to} className={classes.link}>
          <span className={classes.icon} aria-hidden="true">
            <Icon path={item.icon} size={0.85} />
          </span>
          <Text component="span" fw={600} size="sm">
            {item.title}
          </Text>
        </Link>
      ))}
    </nav>
  )
}
