import { AppShell, Box, LoadingOverlay, Stack } from '@mantine/core'
import { useLocalStorage, useMediaQuery } from '@mantine/hooks'
import React, { FC, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { AppFooter } from '@Components/AppFooter'
import { AppHeader } from '@Components/AppHeader'
import { AppNavbar } from '@Components/AppNavbar'
import { CustomColorModal } from '@Components/CustomColorModal'
import { IconHeader } from '@Components/IconHeader'
import {
  deserializeNavigationRailPreference,
  getNavigationRailWidth,
  NAVIGATION_COMPACT_MEDIA_QUERY,
  NAVIGATION_MOBILE_BREAKPOINT,
  NAVIGATION_RAIL_STORAGE_KEY,
  NavigationRailPreference,
  resolveNavigationRailCompact,
  serializeNavigationRailPreference,
  toggleNavigationRailPreference,
} from '@Utils/NavigationRailState'
import { DEFAULT_LOADING_OVERLAY } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import classes from '@Styles/AppNavbar.module.css'

interface WithNavBarProps extends React.PropsWithChildren {
  width?: string
  isLoading?: boolean
  withFooter?: boolean
  withHeader?: boolean
  stickyHeader?: boolean
}

export interface AppControlProps {
  openColorModal: () => void
}

export const WithNavBar: FC<WithNavBarProps> = ({
  children,
  width,
  isLoading,
  withFooter = false,
  withHeader,
  stickyHeader = false,
}) => {
  const isMobile = useIsMobile()
  const { t } = useTranslation()
  const [colorModalOpened, setColorModalOpened] = useState(false)
  const compactViewport = useMediaQuery(NAVIGATION_COMPACT_MEDIA_QUERY, false, {
    getInitialValueInEffect: false,
  })
  const [navigationRailPreference, setNavigationRailPreference] = useLocalStorage<NavigationRailPreference>({
    key: NAVIGATION_RAIL_STORAGE_KEY,
    defaultValue: null,
    getInitialValueInEffect: false,
    deserialize: deserializeNavigationRailPreference,
    serialize: serializeNavigationRailPreference,
  })
  const navigationCompact = resolveNavigationRailCompact(navigationRailPreference, compactViewport)

  const openColorModal = () => setColorModalOpened(true)
  const toggleNavigation = () => setNavigationRailPreference(toggleNavigationRailPreference(navigationCompact))

  return (
    <>
      <a href="#main-content" className={classes.skipLink}>
        {t('common.content.skip_to_main', 'Skip to main content')}
      </a>
      <AppShell
        p={0}
        header={{ height: 68, collapsed: !isMobile }}
        navbar={{
          width: getNavigationRailWidth(navigationCompact),
          breakpoint: NAVIGATION_MOBILE_BREAKPOINT,
          collapsed: {
            mobile: true,
          },
        }}
      >
        <AppHeader openColorModal={openColorModal} />
        <AppNavbar openColorModal={openColorModal} compact={navigationCompact} onToggleCompact={toggleNavigation} />
        <AppShell.Main
          component="main"
          id="main-content"
          tabIndex={-1}
          w="100%"
          aria-busy={isLoading || undefined}
          className={classes.shellMain}
        >
          <Stack data-mobile={isMobile || undefined} data-pb={withFooter || undefined} className={classes.main}>
            <LoadingOverlay visible={isLoading ?? false} overlayProps={DEFAULT_LOADING_OVERLAY} />
            {withHeader && <IconHeader px={isMobile ? '2%' : '10%'} sticky={stickyHeader} />}
            <Box
              className={classes.content}
              style={
                {
                  '--page-content-width': width ?? '1440px',
                  zIndex: 20,
                } as React.CSSProperties
              }
            >
              {children}
            </Box>
            <CustomColorModal opened={colorModalOpened} onClose={() => setColorModalOpened(false)} />
          </Stack>
          {withFooter && <AppFooter />}
        </AppShell.Main>
      </AppShell>
    </>
  )
}
