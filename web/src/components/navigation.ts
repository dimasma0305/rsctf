import {
  mdiAccountGroupOutline,
  mdiFlagOutline,
  mdiHomeVariantOutline,
  mdiInformationOutline,
  mdiNoteTextOutline,
  mdiWrenchOutline,
} from '@mdi/js'
import { ProfileUserInfoModel, Role } from '@Api'

export interface PrimaryNavigationItem {
  icon: string
  label: string
  link: string
  admin?: boolean
}

export const PRIMARY_NAVIGATION: PrimaryNavigationItem[] = [
  { icon: mdiHomeVariantOutline, label: 'common.tab.home', link: '/' },
  { icon: mdiNoteTextOutline, label: 'common.tab.post', link: '/posts' },
  { icon: mdiFlagOutline, label: 'common.tab.game', link: '/games' },
  { icon: mdiAccountGroupOutline, label: 'common.tab.team', link: '/teams' },
  { icon: mdiInformationOutline, label: 'common.tab.about', link: '/about' },
  { icon: mdiWrenchOutline, label: 'common.tab.admin', link: '/admin/games', admin: true },
]

export const canAccessNavigationItem = (item: PrimaryNavigationItem, user?: ProfileUserInfoModel) =>
  !item.admin || user?.role === Role.Admin || user?.hasManagedGames === true

export const isNavigationItemActive = (item: PrimaryNavigationItem, pathname: string) => {
  if (item.link === '/') return pathname === '/'
  if (item.link.startsWith('/admin')) return pathname.startsWith('/admin')
  return pathname.startsWith(item.link)
}
