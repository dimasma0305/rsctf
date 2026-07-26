import { Box, Group, GroupProps, LoadingOverlay, NavLink, Paper, Select, Stack, Text, Title } from '@mantine/core'
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
  mdiServerNetwork,
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
import classes from '@Styles/AdminTabs.module.css'

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
    {
      icon: mdiViewDashboard,
      title: t('admin.title.dashboard', 'Dashboard'),
      description: t(
        'admin.description.dashboard',
        'Monitor participation, submission activity, reviews, and platform health.'
      ),
      path: 'dashboard',
    },
    {
      icon: mdiFlagOutline,
      title: t('admin.tab.games.index'),
      description: t('admin.description.games', 'Create events and manage their challenges, scoring, and access.'),
      path: 'games',
    },
    {
      icon: mdiAccountGroupOutline,
      title: t('admin.tab.teams'),
      description: t('admin.description.teams', 'Review teams, membership, invitations, and participation.'),
      path: 'teams',
    },
    {
      icon: mdiAccountCogOutline,
      title: t('admin.tab.users'),
      description: t('admin.description.users', 'Manage competitor accounts, roles, and access.'),
      path: 'users',
    },
    {
      icon: mdiPackageVariantClosed,
      title: t('admin.tab.instances'),
      description: t('admin.description.instances', 'Inspect and operate active challenge workloads.'),
      path: 'instances',
    },
    {
      icon: mdiServerNetwork,
      title: t('admin.tab.workers', 'Workers'),
      description: t(
        'admin.description.workers',
        'Enroll trusted compute hosts and monitor their readiness and capacity.'
      ),
      path: 'workers',
    },
    {
      icon: mdiSourceBranch,
      title: t('admin.tab.repo_bindings', 'Repo bindings'),
      description: t('admin.description.repo_bindings', 'Synchronize event configuration from connected repositories.'),
      path: 'repo-bindings',
    },
    {
      icon: mdiShieldAlertOutline,
      title: t('admin.tab.anti_cheat', 'Anti-cheat'),
      description: t('admin.description.anti_cheat', 'Investigate suspicious activity and protect event integrity.'),
      path: 'anti-cheat',
    },
    {
      icon: mdiHammerWrench,
      title: t('admin.tab.builds', 'Builds'),
      description: t('admin.description.builds', 'Track challenge image builds and diagnose failures.'),
      path: 'builds',
    },
    {
      icon: mdiFileDocumentOutline,
      title: t('admin.tab.logs'),
      description: t('admin.description.logs', 'Search the operational audit trail and background task history.'),
      path: 'logs',
    },
    {
      icon: mdiSitemapOutline,
      title: t('admin.tab.settings'),
      description: t('admin.description.settings', 'Configure platform-wide behavior, identity, and integrations.'),
      path: 'settings',
    },
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
    <Stack gap="lg" pt="lg" pos="relative" className={classes.page}>
      <Group component="header" justify="space-between" align="flex-end" gap="lg" wrap="wrap">
        <Stack gap={3} className={classes.heading}>
          <Text className={classes.eyebrow}>{t('admin.title.workspace', 'Administration')}</Text>
          <Title order={1} size="h2" className={classes.title}>
            {activePage?.title}
          </Title>
          {activePage?.description && (
            <Text c="dimmed" size="sm" className={classes.description}>
              {activePage.description}
            </Text>
          )}
        </Stack>
      </Group>

      <Paper
        component="nav"
        aria-label={navigationLabel}
        visibleFrom="md"
        withBorder
        radius="lg"
        className={classes.navigation}
      >
        <Box className={classes.navigationViewport}>
          <Group gap={4} wrap="nowrap" className={classes.navigationItems}>
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
                  className={classes.navigationLink}
                />
              )
            })}
          </Group>
        </Box>
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
      {head && (
        <Group
          wrap="wrap"
          justify="space-between"
          mih="44px"
          w="100%"
          gap="sm"
          className={classes.toolbar}
          {...headProps}
        >
          {head}
        </Group>
      )}
      {children}
      <LoadingOverlay visible={isLoading ?? false} overlayProps={DEFAULT_LOADING_OVERLAY} />
    </Stack>
  )
}
