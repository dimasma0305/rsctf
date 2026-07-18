import {
  Group,
  GroupProps,
  LoadingOverlay,
  NavLink,
  Paper,
  Select,
  SimpleGrid,
  Stack,
  Text,
  Title,
} from '@mantine/core'
import {
  mdiAccountCogOutline,
  mdiAccountGroupOutline,
  mdiFileDocumentOutline,
  mdiFlagOutline,
  mdiHammerWrench,
  mdiPackageVariantClosed,
  mdiShieldAlertOutline,
  mdiSitemapOutline,
  mdiSourceBranch,
  mdiViewDashboard,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import React, { FC, useEffect } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useLocation, useNavigate } from 'react-router'
import { DEFAULT_LOADING_OVERLAY } from '@Utils/Shared'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useUser } from '@Hooks/useUser'
import { Role } from '@Api'

export interface AdminTabProps extends React.PropsWithChildren {
  head?: React.ReactNode
  isLoading?: boolean
  headProps?: GroupProps
}

export const WithAdminTab: FC<AdminTabProps> = ({ head, headProps, isLoading, children }) => {
  const navigate = useNavigate()
  const location = useLocation()

  const { t } = useTranslation()

  const pages = [
    { icon: mdiViewDashboard, title: t('admin.title.dashboard', 'Dashboard'), path: 'dashboard' },
    { icon: mdiFlagOutline, title: t('admin.tab.games.index'), path: 'games' },
    { icon: mdiAccountGroupOutline, title: t('admin.tab.teams'), path: 'teams' },
    { icon: mdiAccountCogOutline, title: t('admin.tab.users'), path: 'users' },
    {
      icon: mdiPackageVariantClosed,
      title: t('admin.tab.instances'),
      path: 'instances',
    },
    {
      icon: mdiSourceBranch,
      title: t('admin.tab.repo_bindings', 'Repo bindings'),
      path: 'repo-bindings',
    },
    {
      icon: mdiShieldAlertOutline,
      title: t('admin.tab.anti_cheat', 'Anti-cheat'),
      path: 'anti-cheat',
    },
    {
      icon: mdiHammerWrench,
      title: t('admin.tab.builds', 'Builds'),
      path: 'builds',
    },
    { icon: mdiFileDocumentOutline, title: t('admin.tab.logs'), path: 'logs' },
    { icon: mdiSitemapOutline, title: t('admin.tab.settings'), path: 'settings' },
  ]

  const { user } = useUser()
  const filteredPages = pages.filter(
    (page) => user?.role === Role.Admin || (user?.hasManagedGames && page.path === 'games')
  )

  const getTab = (path: string) => filteredPages.findIndex((page) => path.startsWith(`/admin/${page.path}`))
  const tabIndex = getTab(location.pathname)

  useEffect(() => {
    if (!user) return

    const tab = getTab(location.pathname)
    if (tab < 0) {
      const firstPage = filteredPages[0]
      navigate(firstPage ? `/admin/${firstPage.path}` : '/', { replace: true })
    }
  }, [location.pathname, navigate, user?.role, user?.hasManagedGames])

  usePageTitle(filteredPages[tabIndex]?.title)

  const activePage = filteredPages[tabIndex] ?? filteredPages[0]
  const navigationLabel = t('admin.tab.navigation', 'Administration sections')

  return (
    <Stack gap="sm" pt="md" pos="relative">
      <Paper component="nav" aria-label={navigationLabel} visibleFrom="md" withBorder radius="lg" p={7}>
        <SimpleGrid cols={Math.min(5, Math.max(1, filteredPages.length))} spacing={4}>
          {filteredPages.map((page) => {
            const active = page.path === activePage?.path
            return (
              <NavLink
                key={page.path}
                component={Link}
                to={`/admin/${page.path}`}
                active={active}
                aria-current={active ? 'page' : undefined}
                label={page.title}
                leftSection={<Icon path={page.icon} size={0.9} aria-hidden="true" />}
                variant="light"
                styles={{ root: { borderRadius: 'var(--mantine-radius-md)', minHeight: 44 } }}
              />
            )
          })}
        </SimpleGrid>
      </Paper>
      <Select
        hiddenFrom="md"
        label={t('admin.tab.section_picker', 'Administration section')}
        allowDeselect={false}
        searchable={filteredPages.length > 6}
        value={activePage?.path ?? null}
        data={filteredPages.map((page) => ({ value: page.path, label: page.title }))}
        leftSection={activePage ? <Icon path={activePage.icon} size={0.9} aria-hidden="true" /> : undefined}
        renderOption={({ option }) => {
          const page = filteredPages.find((candidate) => candidate.path === option.value)
          return (
            <Group gap="sm" wrap="nowrap">
              {page && <Icon path={page.icon} size={0.9} aria-hidden="true" />}
              <Text size="sm">{option.label}</Text>
            </Group>
          )
        }}
        onChange={(path) => path && navigate(`/admin/${path}`)}
      />
      <Stack gap={0} pt="xs">
        <Text size="xs" fw={750} tt="uppercase" c="dimmed" style={{ letterSpacing: '0.09em' }}>
          {t('admin.title.workspace', 'Administration')}
        </Text>
        <Title order={1} size="h2">
          {activePage?.title}
        </Title>
      </Stack>
      {head && (
        <Group wrap="wrap" justify="space-between" mih="44px" w="100%" gap="sm" {...headProps}>
          {head}
        </Group>
      )}
      {children}
      <LoadingOverlay visible={isLoading ?? false} overlayProps={DEFAULT_LOADING_OVERLAY} />
    </Stack>
  )
}
