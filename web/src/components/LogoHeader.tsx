import { Group, GroupProps, Title } from '@mantine/core'
import { forwardRef } from 'react'
import { LogoBox } from '@Components/LogoBox'
import { useConfig } from '@Hooks/useConfig'
import classes from '@Styles/LogoHeader.module.css'

export const LogoHeader = forwardRef<HTMLDivElement, GroupProps & { logoSize?: string }>(
  ({ logoSize = '50px', ...props }, ref) => {
    const { config } = useConfig()
    return (
      <Group ref={ref} wrap="nowrap" align="center" justify="flex-start" gap="sm" {...props}>
        <LogoBox size={logoSize} />
        <Title
          component="span"
          textWrap="nowrap"
          className={classes.title}
          lineClamp={1}
          title={`${config?.title?.trim() || 'RS'}::CTF`}
        >
          {config?.title?.trim() || 'RS'}
          <span className={classes.brand}>::</span>CTF
        </Title>
      </Group>
    )
  }
)
