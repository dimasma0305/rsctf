export type NavigationRailPreference = boolean | null

export const NAVIGATION_RAIL_STORAGE_KEY = 'rsctf-navigation-rail-compact'
export const NAVIGATION_RAIL_WIDTH = 76
export const NAVIGATION_SIDEBAR_WIDTH = 260

// Mantine subtracts 0.1px from AppShell mobile breakpoints. Using 768.1 keeps
// the mobile shell active through exactly 768px, matching useIsMobile/CSS.
export const NAVIGATION_MOBILE_BREAKPOINT = 768.1
// Match the theme's 48em–75em shell range. Chromium rounds fractional pixel
// max-width queries up at 1200px, while 74.99em keeps the 75em boundary exact.
export const NAVIGATION_COMPACT_MEDIA_QUERY = '(min-width: 48em) and (max-width: 74.99em)'

export const deserializeNavigationRailPreference = (value?: string): NavigationRailPreference => {
  if (value === 'true') return true
  if (value === 'false') return false
  return null
}

export const serializeNavigationRailPreference = (value: NavigationRailPreference) =>
  value === null ? 'null' : String(value)

export const resolveNavigationRailCompact = (preference: NavigationRailPreference, compactViewport: boolean): boolean =>
  preference ?? compactViewport

export const toggleNavigationRailPreference = (currentlyCompact: boolean): NavigationRailPreference => !currentlyCompact

export const getNavigationRailWidth = (compact: boolean) => (compact ? NAVIGATION_RAIL_WIDTH : NAVIGATION_SIDEBAR_WIDTH)
