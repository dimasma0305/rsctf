import {
  ActionIcon,
  AppShell,
  Avatar,
  Burger,
  Divider,
  Drawer,
  Group,
  Menu,
  Stack,
  Text,
  UnstyledButton,
  useMantineColorScheme,
} from '@mantine/core'
import {
  mdiCached,
  mdiLogin,
  mdiLogout,
  mdiMenu,
  mdiPalette,
  mdiTranslate,
  mdiWeatherNight,
  mdiWeatherSunny,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useLocation } from 'react-router'
import { LogoHeader } from '@Components/LogoHeader'
import { AppControlProps } from '@Components/WithNavbar'
import { PRIMARY_NAVIGATION, canAccessNavigationItem, isNavigationItemActive } from '@Components/navigation'
import { clearLocalCache } from '@Utils/Cache'
import { LanguageMap, SupportedLanguages, useLanguage } from '@Utils/I18n'
import { useLogOut, useUser } from '@Hooks/useUser'
import classes from '@Styles/AppHeader.module.css'

export const AppHeader: FC<AppControlProps> = ({ openColorModal }) => {
  const [opened, setOpened] = useState(false)
  const location = useLocation()
  const { colorScheme, toggleColorScheme } = useMantineColorScheme()
  const { user, error } = useUser()
  const logout = useLogOut()
  const { t } = useTranslation()
  const { language, setLanguage, supportedLanguages } = useLanguage()
  const loggedIn = Boolean(user && !error)

  const close = () => setOpened(false)
  const navItems = PRIMARY_NAVIGATION.filter((item) => canAccessNavigationItem(item, user))
  const dockItems = navItems.filter((item) => !item.admin).slice(0, 4)

  return (
    <>
      <AppShell.Header className={classes.header}>
        <Group h="100%" px="md" justify="space-between" wrap="nowrap">
          <Link to="/" className={classes.brandLink}>
            <LogoHeader />
          </Link>
          <Group justify="flex-end" wrap="nowrap" gap="xs">
            <Menu position="bottom-end" offset={14} width={190}>
              <Menu.Target>
                <ActionIcon size={44} className={classes.button} aria-label={t('common.tab.language', 'Language')}>
                  <Icon path={mdiTranslate} size={0.9} />
                </ActionIcon>
              </Menu.Target>
              <Menu.Dropdown>
                <Menu.Label>{LanguageMap[language] ?? language}</Menu.Label>
                {supportedLanguages.map((lang: SupportedLanguages) => (
                  <Menu.Item key={lang} fw={500} onClick={() => setLanguage(lang)}>
                    {LanguageMap[lang] ?? lang}
                  </Menu.Item>
                ))}
              </Menu.Dropdown>
            </Menu>
            <Burger
              className={classes.burger}
              opened={opened}
              onClick={() => setOpened((value) => !value)}
              aria-label={opened ? t('common.button.close', 'Close menu') : t('common.button.open', 'Open menu')}
              size="sm"
            />
          </Group>
        </Group>
      </AppShell.Header>

      <Drawer
        opened={opened}
        onClose={close}
        position="right"
        size="min(92vw, 360px)"
        title={t('common.tab.navigation', 'Navigation')}
        closeButtonProps={{ 'aria-label': t('common.button.close', 'Close menu') }}
        classNames={{ body: classes.drawerBody, header: classes.drawerHeader }}
      >
        <Stack gap="lg">
          <nav aria-label={t('common.tab.navigation', 'Primary navigation')}>
            <Stack gap={4}>
              {navItems.map((item) => (
                <UnstyledButton
                  key={item.label}
                  component={Link}
                  to={item.link}
                  onClick={close}
                  aria-current={isNavigationItemActive(item, location.pathname) ? 'page' : undefined}
                  data-active={isNavigationItemActive(item, location.pathname) || undefined}
                  className={classes.navLink}
                >
                  <span className={classes.navIcon} aria-hidden="true">
                    <Icon path={item.icon} size={0.95} />
                  </span>
                  <Text component="span" fw={650}>
                    {t(item.label)}
                  </Text>
                </UnstyledButton>
              ))}
            </Stack>
          </nav>

          <Divider label={t('common.tab.account.title', 'Account')} labelPosition="left" />
          <Stack gap={4}>
            {loggedIn ? (
              <>
                <UnstyledButton component={Link} to="/account/profile" onClick={close} className={classes.accountCard}>
                  <Avatar src={user?.avatar} radius="md" size={42}>
                    {user?.userName?.slice(0, 1) ?? 'U'}
                  </Avatar>
                  <Stack gap={0} className={classes.accountText}>
                    <Text fw={700} truncate>
                      {user?.userName}
                    </Text>
                    <Text size="xs" c="dimmed">
                      {t('common.tab.account.profile')}
                    </Text>
                  </Stack>
                </UnstyledButton>
                <UnstyledButton
                  className={classes.navLink}
                  onClick={() => {
                    close()
                    void logout()
                  }}
                >
                  <span className={classes.navIcon} aria-hidden="true">
                    <Icon path={mdiLogout} size={0.95} />
                  </span>
                  <Text component="span" fw={650}>
                    {t('common.tab.account.logout')}
                  </Text>
                </UnstyledButton>
              </>
            ) : (
              <UnstyledButton
                component={Link}
                to={`/account/login?from=${encodeURIComponent(location.pathname + location.search)}`}
                onClick={close}
                className={classes.navLink}
              >
                <span className={classes.navIcon} aria-hidden="true">
                  <Icon path={mdiLogin} size={0.95} />
                </span>
                <Text component="span" fw={650}>
                  {t('common.tab.account.login')}
                </Text>
              </UnstyledButton>
            )}
          </Stack>

          <Divider label={t('common.tab.preferences', 'Preferences')} labelPosition="left" />
          <Stack gap={4}>
            <UnstyledButton className={classes.navLink} onClick={() => toggleColorScheme()}>
              <span className={classes.navIcon} aria-hidden="true">
                <Icon path={colorScheme === 'dark' ? mdiWeatherSunny : mdiWeatherNight} size={0.95} />
              </span>
              <Stack gap={0}>
                <Text component="span" fw={650}>
                  {t('common.tab.theme.title', 'Appearance')}
                </Text>
                <Text component="span" size="xs" c="dimmed">
                  {t('common.tab.theme.switch_to', {
                    theme: colorScheme === 'dark' ? t('common.tab.theme.light') : t('common.tab.theme.dark'),
                  })}
                </Text>
              </Stack>
            </UnstyledButton>
            <UnstyledButton
              className={classes.navLink}
              onClick={() => {
                clearLocalCache()
                close()
              }}
            >
              <span className={classes.navIcon} aria-hidden="true">
                <Icon path={mdiCached} size={0.95} />
              </span>
              <Text component="span" fw={650}>
                {t('common.tab.account.clean_cache')}
              </Text>
            </UnstyledButton>
            <UnstyledButton
              className={classes.navLink}
              onClick={() => {
                openColorModal()
                close()
              }}
            >
              <span className={classes.navIcon} aria-hidden="true">
                <Icon path={mdiPalette} size={0.95} />
              </span>
              <Text component="span" fw={650}>
                {t('common.content.color.title')}
              </Text>
            </UnstyledButton>
          </Stack>
        </Stack>
      </Drawer>

      <nav className={classes.dock} aria-label={t('common.tab.mobile_navigation', 'Mobile navigation')}>
        {dockItems.map((item) => {
          const active = isNavigationItemActive(item, location.pathname)
          return (
            <UnstyledButton
              key={item.label}
              component={Link}
              to={item.link}
              aria-current={active ? 'page' : undefined}
              data-active={active || undefined}
              className={classes.dockItem}
            >
              <span className={classes.dockIcon} aria-hidden="true">
                <Icon path={item.icon} size={0.88} />
              </span>
              <Text component="span" size="xs" fw={650} truncate>
                {t(item.label)}
              </Text>
            </UnstyledButton>
          )
        })}
        <UnstyledButton
          className={classes.dockItem}
          aria-label={t('common.button.more_navigation', 'More navigation options')}
          aria-expanded={opened}
          aria-haspopup="dialog"
          data-active={
            opened || location.pathname.startsWith('/admin') || location.pathname.startsWith('/about') || undefined
          }
          onClick={() => setOpened(true)}
        >
          <span className={classes.dockIcon} aria-hidden="true">
            <Icon path={mdiMenu} size={0.88} />
          </span>
          <Text component="span" size="xs" fw={650}>
            {t('common.button.more', 'More')}
          </Text>
        </UnstyledButton>
      </nav>
    </>
  )
}
