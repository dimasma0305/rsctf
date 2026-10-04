import { Group, GroupProps, LoadingOverlay, Select, Stack } from '@mantine/core'
import React, { FC, useEffect } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, useNavigate } from 'react-router'
import { PageHeader } from '@Components/PageHeader'
import { getAdminEventContext } from '@Components/admin/navigation'
import { getAdminNavigation } from '@Components/navigation'
import { DEFAULT_LOADING_OVERLAY } from '@Utils/Shared'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useUser } from '@Hooks/useUser'
import classes from '@Styles/AdminTabs.module.css'

export interface AdminTabProps extends React.PropsWithChildren {
  head?: React.ReactNode
  isLoading?: boolean
  headProps?: GroupProps
  headerActions?: React.ReactNode
}

export const WithAdminTab: FC<AdminTabProps> = ({ head, headProps, headerActions, isLoading, children }) => {
  const navigate = useNavigate()
  const location = useLocation()
  const { t } = useTranslation()
  const { user } = useUser()
  const pages = getAdminNavigation(user)
  const activePage = pages.find(
    (page) => location.pathname === `/admin/${page.path}` || location.pathname.startsWith(`/admin/${page.path}/`)
  )
  const context = getAdminEventContext(location.pathname)
  const title = context?.challengeId
    ? t(context.flags ? 'admin.navigation.flags_files' : 'admin.navigation.challenge_settings')
    : context?.section
      ? t(context.section.label, context.section.fallback)
      : activePage
        ? t(activePage.label, activePage.fallback)
        : t('common.workspace.admin', 'Administration')
  usePageTitle(title)

  useEffect(() => {
    if (user && !activePage) navigate(pages[0] ? `/admin/${pages[0].path}` : '/', { replace: true })
  }, [location.pathname, user?.role, user?.hasManagedGames, navigate, activePage])

  return (
    <Stack gap="lg" pos="relative" className={classes.page} data-admin-workspace>
      <PageHeader
        eyebrow={
          context
            ? t('common.workspace.event_id', 'Event #{{id}}', { id: context.id })
            : t('common.workspace.admin', 'Administration')
        }
        title={title}
        description={
          !context && activePage ? t(`admin.description.${activePage.path.replaceAll('-', '_')}`, '') : undefined
        }
        actions={headerActions}
      />
      {!context && (
        <Select
          hiddenFrom="sm"
          label={t('common.workspace.admin_section', 'Administration section')}
          allowDeselect={false}
          searchable={pages.length > 6}
          value={activePage?.path ?? null}
          data={pages.map((page) => ({ value: page.path, label: t(page.label, page.fallback) }))}
          onChange={(path) => path && navigate(`/admin/${path}`)}
        />
      )}
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
