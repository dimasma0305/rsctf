import { Button, Flex, Group, GroupProps, LoadingOverlay, NavLink, Select, Stack, Text } from '@mantine/core'
import {
  mdiAccountGroupOutline,
  mdiBullhornOutline,
  mdiClockOutline,
  mdiFileDocumentCheckOutline,
  mdiFlagVariantOutline,
  mdiFlagOutline,
  mdiKeyboardBackspace,
  mdiTagOutline,
  mdiTextBoxOutline,
  mdiAccountKey,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import React, { FC, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, Link, useNavigate, useParams } from 'react-router'
import { AdminPage } from '@Components/admin/AdminPage'
import { ChallengeConsoleTabs } from '@Components/admin/ChallengeConsoleTabs'
import { DEFAULT_LOADING_OVERLAY } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import { useUser } from '@Hooks/useUser'
import { Role } from '@Api'
import misc from '@Styles/Misc.module.css'

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
  backUrl,
  ...others
}) => {
  const navigate = useNavigate()
  const location = useLocation()
  const { id } = useParams()
  const { t } = useTranslation()
  const { user } = useUser()
  const isAdmin = user?.role === Role.Admin
  const isCompact = useIsMobile(1100)

  const pages = [
    { icon: mdiTextBoxOutline, title: t('admin.tab.games.info'), path: 'info' },
    { icon: mdiBullhornOutline, title: t('admin.tab.games.notices'), path: 'notices' },
    // 'pending' must precede 'challenges' so the fuzzy path.includes match
    // resolves /challenges/pending to this tab instead of plain Challenges.
    { icon: mdiClockOutline, title: t('admin.tab.games.pending', 'Pending'), path: 'pending' },
    { icon: mdiFlagOutline, title: t('admin.tab.games.challenges'), path: 'challenges' },
    { icon: mdiTagOutline, title: t('admin.tab.games.divisions'), path: 'divisions' },
    { icon: mdiAccountGroupOutline, title: t('admin.tab.games.review'), path: 'review' },
    { icon: mdiFileDocumentCheckOutline, title: t('admin.tab.games.writeups'), path: 'writeups' },
    { icon: mdiFlagVariantOutline, title: t('admin.tab.games.flag_egress', 'Flag Egress'), path: 'flagegress' },
    { icon: mdiAccountKey, title: t('admin.tab.games.managers', 'Managers'), path: 'managers', adminOnly: true },
  ].filter((p) => isAdmin || !p.adminOnly)

  // `challengereviews` + `adops` folded into the Challenges console (a sub-nav),
  // so the Challenges sidebar tab stays highlighted for all three views.
  const getTab = (path: string) =>
    path.includes('adops') || path.includes('challengereviews')
      ? pages.find((page) => page.path === 'challenges')
      : pages.find((page) => path.includes(page.path))

  const [activeTab, setActiveTab] = useState(getTab(location.pathname)?.path ?? pages[0].path)

  useEffect(() => {
    if (!user) return

    const tab = getTab(location.pathname)
    if (tab) {
      setActiveTab(tab.path ?? '')
    } else if (pages.length > 0) {
      navigate(`/admin/games/${id}/${pages[0].path}`)
    }
  }, [location.pathname, id, navigate, user?.role])

  return (
    <AdminPage
      {...others}
      head={
        <>
          <Button
            variant="default"
            w={isCompact ? 'auto' : '10rem'}
            component={Link}
            classNames={{ inner: misc.justifyBetween }}
            leftSection={<Icon path={mdiKeyboardBackspace} size={1} />}
            to={backUrl ?? '/admin/games'}
          >
            {t('admin.button.back')}
          </Button>
          <Group wrap="wrap" justify={contentPos ?? 'space-between'} miw={0} style={{ flex: 1 }}>
            {head}
          </Group>
        </>
      }
    >
      <Flex
        direction={isCompact ? 'column' : 'row'}
        gap="md"
        justify="space-between"
        align="flex-start"
        w="100%"
        pb="xl"
      >
        {isCompact ? (
          <Select
            w="100%"
            label={t('admin.tab.games.navigation', 'Game administration sections')}
            value={activeTab}
            allowDeselect={false}
            searchable
            data={pages.map((page) => ({ value: page.path, label: page.title }))}
            onChange={(path) => path && navigate(`/admin/games/${id}/${path}`)}
          />
        ) : (
          <Stack
            component="nav"
            aria-label={t('admin.tab.games.navigation', 'Game administration sections')}
            gap={4}
            w="11rem"
            style={{ flexShrink: 0 }}
          >
            <Text size="xs" fw={650} c="dimmed" px="sm" mb="xs">
              {t('common.workspace.event_id', 'Event #{{id}}', { id })}
            </Text>
            <Button component={Link} to={`/games/${id}`} variant="default" size="xs" mb="sm">
              {t('common.workspace.view_event', 'View event as player')}
            </Button>
            {pages.map((page) => (
              <NavLink
                key={page.path}
                component={Link}
                to={`/admin/games/${id}/${page.path}`}
                active={page.path === activeTab}
                aria-current={page.path === activeTab ? 'page' : undefined}
                label={page.title}
                leftSection={<Icon path={page.icon} size={0.9} />}
                variant="light"
                styles={{ root: { borderRadius: 'var(--mantine-radius-md)', minHeight: 44 } }}
              />
            ))}
          </Stack>
        )}
        <Stack w="100%" miw={0} style={{ flex: 1 }} pos="relative">
          <LoadingOverlay visible={isLoading ?? false} overlayProps={DEFAULT_LOADING_OVERLAY} />
          {/* One challenge console for the three folded views (list / reviews / A&D
              ops) — shown only on their landing routes, not challenge detail pages. */}
          {/\/(challenges|challengereviews|adops)\/?$/.test(location.pathname) && <ChallengeConsoleTabs />}
          {children}
        </Stack>
      </Flex>
    </AdminPage>
  )
}
