import { Group, GroupProps, MantineColor, useMantineColorScheme, useMantineTheme } from '@mantine/core'
import { clamp } from '@mantine/hooks'
import React, { FC, useEffect, useRef, useState } from 'react'
import { Link } from 'react-router'
import { LogoHeader } from '@Components/LogoHeader'
import classes from '@Styles/IconTabs.module.css'

interface TabProps {
  tabKey: string
  color?: MantineColor
  icon?: React.ReactNode
  label?: React.ReactNode
  to?: string
}

interface IconTabsProps extends GroupProps {
  position?: GroupProps['justify']
  tabs: TabProps[]
  grow?: boolean
  active?: number
  withIcon?: boolean
  disabled?: boolean
  aside?: React.ReactNode
  ariaLabel?: string
  mode?: 'tabs' | 'navigation'
  idPrefix?: string
  onTabChange?: (tabIndex: number, tabKey: string) => void
}

interface TabButtonProps extends TabProps {
  active: boolean
  disabled?: boolean
  onClick?: () => void
  onKeyDown?: (event: React.KeyboardEvent<HTMLButtonElement>) => void
  buttonRef?: (node: HTMLButtonElement | null) => void
  idPrefix?: string
}

const TabContent: FC<Pick<TabButtonProps, 'icon' | 'label'>> = ({ icon, label }) => (
  <span className={classes.inner}>
    {icon && (
      <span className={classes.icon} aria-hidden="true">
        {icon}
      </span>
    )}
    {label && <span className={classes.label}>{label}</span>}
  </span>
)

const Tab: FC<TabButtonProps> = ({
  tabKey,
  color,
  label,
  active,
  icon,
  disabled,
  onClick,
  onKeyDown,
  buttonRef,
  idPrefix,
}) => (
  <button
    ref={buttonRef}
    type="button"
    role="tab"
    aria-selected={active}
    aria-controls={idPrefix ? `${idPrefix}-panel` : undefined}
    id={idPrefix ? `${idPrefix}-tab-${tabKey}` : undefined}
    tabIndex={active ? 0 : -1}
    disabled={disabled}
    onClick={onClick}
    onKeyDown={onKeyDown}
    style={{ '--tab-active-color': color } as React.CSSProperties}
    data-active={active || undefined}
    className={classes.default}
  >
    <TabContent icon={icon} label={label} />
  </button>
)

const NavigationTab: FC<TabButtonProps> = ({ to, color, label, active, icon, disabled, onClick }) => (
  <Link
    to={to ?? '#'}
    aria-current={active ? 'page' : undefined}
    aria-disabled={disabled || undefined}
    tabIndex={disabled ? -1 : undefined}
    onClick={(event) => {
      if (disabled) {
        event.preventDefault()
        return
      }
      onClick?.()
    }}
    style={{ '--tab-active-color': color } as React.CSSProperties}
    data-active={active || undefined}
    className={classes.default}
  >
    <TabContent icon={icon} label={label} />
  </Link>
)

export const IconTabs: FC<IconTabsProps> = (props) => {
  const {
    active,
    onTabChange,
    tabs,
    withIcon,
    aside,
    disabled,
    ariaLabel = 'Section navigation',
    mode = 'tabs',
    idPrefix,
    position,
    grow,
    ...others
  } = props
  const [activeTab, setActiveTab] = useState(active ?? 0)
  const tabRefs = useRef<Array<HTMLButtonElement | null>>([])
  const scrollerRef = useRef<HTMLDivElement>(null)
  const theme = useMantineTheme()
  const { colorScheme } = useMantineColorScheme()
  const resolveColor = (color?: MantineColor) =>
    color ? theme.colors[theme.primaryColor][colorScheme === 'dark' ? 4 : 7] : undefined
  const current = tabs.length > 0 ? clamp(activeTab, 0, tabs.length - 1) : -1

  useEffect(() => {
    setActiveTab(active ?? 0)
  }, [active])

  useEffect(() => {
    const scroller = scrollerRef.current
    const activeItem = scroller?.querySelector<HTMLElement>('[data-active]')
    if (!scroller || !activeItem) return

    const target = activeItem.offsetLeft - (scroller.clientWidth - activeItem.offsetWidth) / 2
    scroller.scrollTo({ left: Math.max(0, target), behavior: 'smooth' })
  }, [current, mode, tabs.length])

  const selectTab = (index: number, focus = false) => {
    const tab = tabs[index]
    if (!tab || disabled) return
    setActiveTab(index)
    onTabChange?.(index, tab.tabKey)
    if (focus) window.requestAnimationFrame(() => tabRefs.current[index]?.focus())
  }

  const onKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>, index: number) => {
    let next = index
    if (event.key === 'ArrowLeft') next = index === 0 ? tabs.length - 1 : index - 1
    else if (event.key === 'ArrowRight') next = index === tabs.length - 1 ? 0 : index + 1
    else if (event.key === 'Home') next = 0
    else if (event.key === 'End') next = tabs.length - 1
    else return

    event.preventDefault()
    selectTab(next, true)
  }

  return (
    <div className={classes.root}>
      {(withIcon || aside) && (
        <div className={classes.context}>
          {withIcon && <LogoHeader className={classes.hidable} />}
          {aside}
        </div>
      )}
      <div ref={scrollerRef} className={classes.scroller}>
        <Group
          component={mode === 'navigation' ? 'nav' : 'div'}
          role={mode === 'tabs' ? 'tablist' : undefined}
          aria-label={ariaLabel}
          gap={4}
          wrap="nowrap"
          justify={position}
          grow={grow}
          className={classes.panes}
          {...others}
        >
          {tabs.map((tab, index) => {
            const sharedProps: TabButtonProps = {
              ...tab,
              disabled,
              color: resolveColor(tab.color),
              active: current === index,
              onClick: () => selectTab(index),
            }

            return mode === 'navigation' ? (
              <NavigationTab key={tab.tabKey} {...sharedProps} />
            ) : (
              <Tab
                key={tab.tabKey}
                {...sharedProps}
                idPrefix={idPrefix}
                buttonRef={(node) => {
                  tabRefs.current[index] = node
                }}
                onKeyDown={(event) => onKeyDown(event, index)}
              />
            )
          })}
        </Group>
      </div>
    </div>
  )
}
