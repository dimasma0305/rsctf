import {
  mdiAccountGroupOutline,
  mdiBookOpenPageVariantOutline,
  mdiFlagOutline,
  mdiFormatListChecks,
  mdiHandHeart,
  mdiHomeVariantOutline,
  mdiInformationOutline,
  mdiNoteTextOutline,
  mdiWrenchOutline,
  mdiViewDashboard,
  mdiAccountCogOutline,
  mdiPackageVariantClosed,
  mdiServerNetwork,
  mdiSourceBranch,
  mdiShieldAlertOutline,
  mdiHammerWrench,
  mdiFileDocumentOutline,
  mdiSitemapOutline,
} from '@mdi/js'
import { ProfileUserInfoModel, Role } from '@Api'

export interface PrimaryNavigationItem {
  icon: string
  label: string
  dockLabel?: string
  link: string
  admin?: boolean
  requiresDonations?: boolean
  requiresAuth?: boolean
  group?: 'workspace' | 'community' | 'manage' | 'operate' | 'platform'
}

export const PRIMARY_NAVIGATION: PrimaryNavigationItem[] = [
  { icon: mdiHomeVariantOutline, label: 'common.tab.home', link: '/' },
  { icon: mdiNoteTextOutline, label: 'common.tab.post', link: '/posts' },
  { icon: mdiFlagOutline, label: 'common.tab.game', link: '/games' },
  {
    icon: mdiFormatListChecks,
    label: 'common.tab.challenge_catalog',
    dockLabel: 'common.tab.challenge_catalog_short',
    link: '/challenges',
    requiresAuth: true,
  },
  { icon: mdiAccountGroupOutline, label: 'common.tab.team', link: '/teams' },
  { icon: mdiBookOpenPageVariantOutline, label: 'common.tab.guide', link: '/guide' },
  {
    icon: mdiHandHeart,
    label: 'common.tab.donations',
    link: '/donations',
    requiresDonations: true,
  },
  { icon: mdiInformationOutline, label: 'common.tab.about', link: '/about' },
  { icon: mdiWrenchOutline, label: 'common.tab.admin', link: '/admin/games', admin: true },
]

export const canAccessNavigationItem = (
  item: PrimaryNavigationItem,
  user?: ProfileUserInfoModel,
  donationsEnabled = false
) =>
  (!item.admin || user?.role === Role.Admin || user?.hasManagedGames === true) &&
  (!item.requiresAuth || Boolean(user)) &&
  (!item.requiresDonations || donationsEnabled)

export const isNavigationItemActive = (item: PrimaryNavigationItem, pathname: string) => {
  if (item.link === '/') return pathname === '/'
  if (item.admin) return pathname.startsWith('/admin/')
  return pathname === item.link || pathname.startsWith(`${item.link}/`)
}

/** One registry for the administration rail, mobile picker and quick navigation.
 * Visibility is a convenience only; every destination remains backend-authorized. */
export const ADMIN_NAVIGATION = [
  {
    icon: mdiViewDashboard,
    path: 'dashboard',
    label: 'common.workspace.dashboard',
    fallback: 'Dashboard',
    group: 'manage',
  },
  { icon: mdiFlagOutline, path: 'games', label: 'admin.tab.games.index', fallback: 'Events', group: 'manage' },
  { icon: mdiAccountGroupOutline, path: 'teams', label: 'admin.tab.teams', fallback: 'Teams', group: 'manage' },
  { icon: mdiAccountCogOutline, path: 'users', label: 'admin.tab.users', fallback: 'Users', group: 'manage' },
  {
    icon: mdiPackageVariantClosed,
    path: 'instances',
    label: 'admin.tab.instances',
    fallback: 'Instances',
    group: 'operate',
  },
  { icon: mdiServerNetwork, path: 'workers', label: 'admin.tab.workers', fallback: 'Workers', group: 'operate' },
  {
    icon: mdiSourceBranch,
    path: 'repo-bindings',
    label: 'admin.tab.repo_bindings',
    fallback: 'Repositories',
    group: 'operate',
  },
  { icon: mdiHammerWrench, path: 'builds', label: 'admin.tab.builds', fallback: 'Builds', group: 'operate' },
  {
    icon: mdiShieldAlertOutline,
    path: 'anti-cheat',
    label: 'admin.tab.anti_cheat',
    fallback: 'Anti-cheat',
    group: 'operate',
  },
  { icon: mdiFileDocumentOutline, path: 'logs', label: 'admin.tab.logs', fallback: 'Logs', group: 'platform' },
  { icon: mdiSitemapOutline, path: 'settings', label: 'admin.tab.settings', fallback: 'Settings', group: 'platform' },
] as const

export const getAdminNavigation = (user?: ProfileUserInfoModel) =>
  ADMIN_NAVIGATION.filter((item) => user?.role === Role.Admin || (user?.hasManagedGames && item.path === 'games'))

export const navigationGroup = (item: PrimaryNavigationItem) =>
  item.group ?? (['/posts', '/guide', '/about', '/donations'].includes(item.link) ? 'community' : 'workspace')

export const getWorkspaceNavigation = (
  pathname: string,
  user?: ProfileUserInfoModel,
  donationsEnabled = false
): PrimaryNavigationItem[] => {
  if (pathname.startsWith('/admin/') && getAdminNavigation(user).length) {
    return getAdminNavigation(user).map((item) => ({ ...item, link: `/admin/${item.path}` }))
  }
  return PRIMARY_NAVIGATION.filter((item) => canAccessNavigationItem(item, user, donationsEnabled)).sort(
    (a, b) => Number(navigationGroup(a) === 'community') - Number(navigationGroup(b) === 'community')
  )
}
