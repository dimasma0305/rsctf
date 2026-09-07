import {
  ActionIcon,
  AppShell,
  Avatar,
  Divider,
  Menu,
  MenuDivider,
  Popover,
  Stack,
  Text,
  Tooltip,
  UnstyledButton,
  useMantineColorScheme,
} from '@mantine/core'
import {
  mdiAccountCircleOutline,
  mdiCached,
  mdiChevronDoubleLeft,
  mdiChevronDoubleRight,
  mdiLogin,
  mdiLogout,
  mdiPalette,
  mdiTranslate,
  mdiTransitConnectionVariant,
  mdiWeatherNight,
  mdiWeatherSunny,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useLocation } from 'react-router'
import { LogoBox } from '@Components/LogoBox'
import { LogoHeader } from '@Components/LogoHeader'
import { WsrxManager } from '@Components/WsrxManager'
import { getWorkspaceNavigation, isNavigationItemActive, navigationGroup } from '@Components/navigation'
import { clearLocalCache } from '@Utils/Cache'
import { LanguageMap, SupportedLanguages, useLanguage } from '@Utils/I18n'
import { useConfig } from '@Hooks/useConfig'
import { useLogOut, useUser } from '@Hooks/useUser'
import { ContainerPortMappingType } from '@Api'
import classes from '@Styles/AppNavbar.module.css'

export interface NavbarLinkProps {
  icon: string
  label: string
  link?: string
  onClick?: () => void
  isActive?: boolean
  compact?: boolean
}

const NavbarLink: FC<NavbarLinkProps> = ({ icon, label, link, onClick, isActive, compact = false }) => {
  const { t } = useTranslation()
  const translatedLabel = t(label)
  const content = (
    <>
      <span className={classes.linkIcon} aria-hidden="true">
        <Icon path={icon} size={0.92} />
      </span>
      {!compact && (
        <Text component="span" size="sm" fw={550} style={{ lineHeight: 1.35, overflowWrap: 'anywhere' }}>
          {translatedLabel}
        </Text>
      )}
    </>
  )

  const control = link ? (
    <UnstyledButton
      component={Link}
      to={link}
      onClick={onClick}
      aria-label={compact ? translatedLabel : undefined}
      aria-current={isActive ? 'page' : undefined}
      data-active={isActive || undefined}
      data-guide={
        link === '/games'
          ? 'games-navigation'
          : link === '/challenges'
            ? 'challenge-navigation'
            : link === '/teams'
              ? 'team-navigation'
              : link === '/guide'
                ? 'guide-navigation'
                : undefined
      }
      className={classes.link}
    >
      {content}
    </UnstyledButton>
  ) : (
    <UnstyledButton
      onClick={onClick}
      aria-label={compact ? translatedLabel : undefined}
      data-active={isActive || undefined}
      className={classes.link}
    >
      {content}
    </UnstyledButton>
  )

  return (
    <Tooltip label={translatedLabel} position="right" withinPortal openDelay={350} disabled={!compact}>
      {control}
    </Tooltip>
  )
}

interface AppNavbarProps {
  openColorModal: () => void
  compact: boolean
  onToggleCompact: () => void
}

export const AppNavbar: FC<AppNavbarProps> = ({ openColorModal, compact, onToggleCompact }) => {
  const location = useLocation()
  const { colorScheme, toggleColorScheme } = useMantineColorScheme()
  const logout = useLogOut()
  const { user, error } = useUser()
  const { config } = useConfig()
  const { t } = useTranslation()
  const { language, setLanguage, supportedLanguages } = useLanguage()

  const items = getWorkspaceNavigation(location.pathname, user, config.donationsEnabled)
  const groups = [...new Set(items.map(navigationGroup))]
  const adminWorkspace = location.pathname.startsWith('/admin/')
  const loggedIn = Boolean(user && !error)
  const toggleLabel = compact
    ? t('common.button.expand_navigation', 'Expand navigation')
    : t('common.button.collapse_navigation', 'Collapse navigation')

  return (
    <AppShell.Navbar
      id="primary-navigation-rail"
      className={classes.navbar}
      aria-label={t('common.tab.navigation', 'Primary navigation')}
      data-compact={compact || undefined}
    >
      <AppShell.Section className={classes.brandSection}>
        <div className={classes.brandRow}>
          <Tooltip
            label={t('common.tab.home', 'Home')}
            position="right"
            withinPortal
            openDelay={350}
            disabled={!compact}
          >
            <Link to="/" className={classes.brandLink} aria-label={t('common.tab.home', 'Home')}>
              {compact ? <LogoBox size="36px" /> : <LogoHeader logoSize="36px" />}
            </Link>
          </Tooltip>
          <Tooltip label={toggleLabel} position={compact ? 'right' : 'bottom'} withinPortal openDelay={350}>
            <ActionIcon
              id="navigation-rail-toggle"
              variant="subtle"
              size={44}
              className={classes.railToggle}
              onClick={onToggleCompact}
              aria-label={toggleLabel}
              aria-expanded={!compact}
              aria-controls="primary-navigation-rail"
            >
              <Icon path={compact ? mdiChevronDoubleRight : mdiChevronDoubleLeft} size={0.86} />
            </ActionIcon>
          </Tooltip>
        </div>
        {!compact && (
          <Text size="xs" c="dimmed" className={classes.workspaceLabel}>
            {config?.slogan?.trim() || 'Capture. Compete. Conquer.'}
          </Text>
        )}
      </AppShell.Section>

      <Divider />

      <AppShell.Section grow className={classes.navigationSection}>
        {adminWorkspace && (
          <NavbarLink
            icon={mdiChevronDoubleLeft}
            label="common.workspace.back_to_player"
            link="/games"
            compact={compact}
          />
        )}
        {groups.map((group) => (
          <Stack key={group} gap={4} className={classes.navigationGroup}>
            {!compact && (
              <Text className={classes.sectionLabel} component="span">
                {t(`common.workspace.groups.${group}`, group)}
              </Text>
            )}
            {items
              .filter((item) => navigationGroup(item) === group)
              .map((item) => (
                <NavbarLink
                  key={item.link}
                  {...item}
                  compact={compact}
                  isActive={isNavigationItemActive(item, location.pathname)}
                />
              ))}
          </Stack>
        ))}
      </AppShell.Section>

      <AppShell.Section className={classes.utilitySection}>
        {!compact && (
          <Text className={classes.sectionLabel} component="span">
            {t('common.tab.preferences', 'Preferences')}
          </Text>
        )}
        <Stack gap={4}>
          {config.portMapping === ContainerPortMappingType.PlatformProxy && (
            <Popover position="right-end" offset={18} width={340}>
              <Popover.Target>
                <UnstyledButton
                  className={classes.link}
                  data-guide="connection-tools"
                  aria-label={compact ? t('common.tab.wsrx', 'Connection tools') : undefined}
                  title={compact ? t('common.tab.wsrx', 'Connection tools') : undefined}
                >
                  <span className={classes.linkIcon} aria-hidden="true">
                    <Icon path={mdiTransitConnectionVariant} size={0.92} />
                  </span>
                  {!compact && (
                    <Text component="span" size="sm" fw={650} truncate>
                      {t('common.tab.wsrx', 'Connection tools')}
                    </Text>
                  )}
                </UnstyledButton>
              </Popover.Target>
              <Popover.Dropdown>
                <WsrxManager />
              </Popover.Dropdown>
            </Popover>
          )}

          <Menu position="right-end" offset={18} width={210}>
            <Menu.Target>
              <UnstyledButton
                className={classes.link}
                aria-label={t('common.tab.language', 'Language')}
                title={compact ? t('common.tab.language', 'Language') : undefined}
              >
                <span className={classes.linkIcon} aria-hidden="true">
                  <Icon path={mdiTranslate} size={0.92} />
                </span>
                {!compact && (
                  <Stack gap={0} className={classes.linkText}>
                    <Text component="span" size="sm" fw={650} truncate>
                      {t('common.tab.language', 'Language')}
                    </Text>
                    <Text component="span" size="xs" c="dimmed" truncate>
                      {LanguageMap[language] ?? language}
                    </Text>
                  </Stack>
                )}
              </UnstyledButton>
            </Menu.Target>
            <Menu.Dropdown>
              <Menu.Label>{t('common.tab.language', 'Language')}</Menu.Label>
              {supportedLanguages.map((lang: SupportedLanguages) => (
                <Menu.Item key={lang} fw={500} onClick={() => setLanguage(lang)}>
                  {LanguageMap[lang] ?? lang}
                </Menu.Item>
              ))}
            </Menu.Dropdown>
          </Menu>

          <Tooltip
            label={t('common.tab.theme.title', 'Appearance')}
            position="right"
            withinPortal
            openDelay={350}
            disabled={!compact}
          >
            <UnstyledButton
              onClick={() => toggleColorScheme()}
              className={classes.link}
              aria-label={compact ? t('common.tab.theme.title', 'Appearance') : undefined}
            >
              <span className={classes.linkIcon} aria-hidden="true">
                <Icon path={colorScheme === 'dark' ? mdiWeatherSunny : mdiWeatherNight} size={0.92} />
              </span>
              {!compact && (
                <Stack gap={0} className={classes.linkText}>
                  <Text component="span" size="sm" fw={650} truncate>
                    {t('common.tab.theme.title', 'Appearance')}
                  </Text>
                  <Text component="span" size="xs" c="dimmed" truncate>
                    {colorScheme === 'dark' ? t('common.tab.theme.dark') : t('common.tab.theme.light')}
                  </Text>
                </Stack>
              )}
            </UnstyledButton>
          </Tooltip>

          <Menu position="right-end" offset={18} width={240} withinPortal={false} withInitialFocusPlaceholder={false}>
            <Menu.Target>
              <UnstyledButton
                className={classes.accountButton}
                data-guide="account-menu"
                aria-label={t('common.tab.account.title', 'Account')}
                title={compact ? t('common.tab.account.title', 'Account') : undefined}
              >
                <Avatar src={user?.avatar} radius="md" size={36}>
                  {user?.userName?.slice(0, 1) ?? <Icon path={mdiAccountCircleOutline} size={0.9} />}
                </Avatar>
                {!compact && (
                  <Stack gap={1} className={classes.linkText}>
                    <Text component="span" size="sm" fw={700} truncate>
                      {loggedIn ? user?.userName : t('common.tab.account.login')}
                    </Text>
                    <Text component="span" size="xs" c="dimmed" truncate>
                      {loggedIn ? t('common.tab.account.profile') : t('common.tab.account.title', 'Account')}
                    </Text>
                  </Stack>
                )}
              </UnstyledButton>
            </Menu.Target>
            <Menu.Dropdown>
              {loggedIn && (
                <Menu.Item
                  component={Link}
                  to="/account/profile"
                  leftSection={<Icon path={mdiAccountCircleOutline} size={0.9} />}
                  data-guide="account-profile"
                >
                  {t('common.tab.account.profile')}
                </Menu.Item>
              )}
              <Menu.Item onClick={clearLocalCache} leftSection={<Icon path={mdiCached} size={0.9} />}>
                {t('common.tab.account.clean_cache')}
              </Menu.Item>
              <Menu.Item onClick={openColorModal} leftSection={<Icon path={mdiPalette} size={0.9} />}>
                {t('common.content.color.title')}
              </Menu.Item>
              <MenuDivider />
              {loggedIn ? (
                <Menu.Item color="red" onClick={logout} leftSection={<Icon path={mdiLogout} size={0.9} />}>
                  {t('common.tab.account.logout')}
                </Menu.Item>
              ) : (
                <Menu.Item
                  component={Link}
                  to={`/account/login?from=${encodeURIComponent(location.pathname + location.search)}`}
                  leftSection={<Icon path={mdiLogin} size={0.9} />}
                  data-guide="account-login"
                >
                  {t('common.tab.account.login')}
                </Menu.Item>
              )}
            </Menu.Dropdown>
          </Menu>
        </Stack>
      </AppShell.Section>
    </AppShell.Navbar>
  )
}
