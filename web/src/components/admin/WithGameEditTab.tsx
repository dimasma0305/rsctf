import { Button, Group, GroupProps, LoadingOverlay, NavLink, Select, Stack, Text } from '@mantine/core'
import { mdiArrowLeft, mdiOpenInNew } from '@mdi/js'
import { Icon } from '@mdi/react'
import React, { FC, useEffect } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, Link, useNavigate, useParams } from 'react-router'
import { AdminPage } from '@Components/admin/AdminPage'
import { getAdminEventContext, getEventAdminSections } from '@Components/admin/navigation'
import { DEFAULT_LOADING_OVERLAY } from '@Utils/Shared'
import { useUser } from '@Hooks/useUser'
import classes from '@Styles/AdminTabs.module.css'

export interface GameEditTabProps extends React.PropsWithChildren {
  head?: React.ReactNode
  headProps?: GroupProps
  contentPos?: React.CSSProperties['justifyContent']
  isLoading?: boolean
  backUrl?: string
}

export const WithGameEditTab: FC<GameEditTabProps> = ({
  children,
  isLoading,
  contentPos,
  head,
  headProps,
  backUrl,
}) => {
  const navigate = useNavigate()
  const { pathname } = useLocation()
  const { id } = useParams()
  const { t } = useTranslation()
  const { user } = useUser()
  const pages = getEventAdminSections(user)
  const context = getAdminEventContext(pathname)
  const activeTab = context?.section?.path
  const groups = ['setup', 'competition', 'people'] as const

  useEffect(() => {
    if (pages.length && !pages.some((page) => page.path === activeTab)) {
      navigate(`/admin/games/${id}/info`, { replace: true })
    }
  }, [pathname, id, navigate, user?.role, user?.hasManagedGames, activeTab])

  return (
    <AdminPage
      headerActions={
        <>
          <Button
            variant="default"
            component={Link}
            leftSection={<Icon path={mdiArrowLeft} size={0.85} />}
            to={backUrl ?? '/admin/games'}
          >
            {t(backUrl?.endsWith('/challenges') ? 'admin.navigation.all_challenges' : 'admin.navigation.all_events')}
          </Button>
          <Button
            component={Link}
            to={`/games/${id}`}
            variant="default"
            leftSection={<Icon path={mdiOpenInNew} size={0.85} />}
            aria-label={t('common.workspace.view_event', 'View event as player')}
          >
            {t('admin.navigation.player_view')}
          </Button>
        </>
      }
    >
      <div className={classes.eventLayout} data-admin-event-navigation>
        <div className={classes.eventMobile}>
          <Select
            label={t('admin.tab.games.navigation', 'Game administration sections')}
            value={activeTab ?? null}
            allowDeselect={false}
            searchable
            data={groups.map((group) => ({
              group: t(`admin.navigation.groups.${group}`),
              items: pages
                .filter((page) => page.group === group)
                .map((page) => ({ value: page.path, label: t(page.label, page.fallback) })),
            }))}
            onChange={(path) => path && navigate(`/admin/games/${id}/${path}`)}
          />
        </div>
        <Stack
          component="nav"
          className={classes.eventNav}
          aria-label={t('admin.tab.games.navigation', 'Game administration sections')}
          gap="md"
        >
          {groups.map((group) => (
            <Stack key={group} gap={3}>
              <Text size="xs" fw={650} c="dimmed" px="sm">
                {t(`admin.navigation.groups.${group}`)}
              </Text>
              {pages
                .filter((page) => page.group === group)
                .map((page) => (
                  <NavLink
                    key={page.path}
                    component={Link}
                    to={`/admin/games/${id}/${page.path}`}
                    active={page.path === activeTab}
                    aria-current={page.path === activeTab ? 'page' : undefined}
                    label={t(page.label, page.fallback)}
                    leftSection={<Icon path={page.icon} size={0.9} aria-hidden="true" />}
                    variant="light"
                    className={classes.eventLink}
                  />
                ))}
            </Stack>
          ))}
        </Stack>
        <Stack miw={0} pos="relative" className={classes.eventContent}>
          <LoadingOverlay visible={isLoading ?? false} overlayProps={DEFAULT_LOADING_OVERLAY} />
          {head && (
            <Group
              wrap="wrap"
              justify={contentPos ?? 'space-between'}
              gap="sm"
              className={classes.toolbar}
              {...headProps}
            >
              {head}
            </Group>
          )}
          {children}
        </Stack>
      </div>
    </AdminPage>
  )
}
