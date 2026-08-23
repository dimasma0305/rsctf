import {
  mdiAccountGroupOutline,
  mdiFlagOutline,
  mdiHandHeart,
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
  requiresDonations?: boolean
}

export const PRIMARY_NAVIGATION: PrimaryNavigationItem[] = [
  { icon: mdiHomeVariantOutline, label: 'common.tab.home', link: '/' },
  { icon: mdiNoteTextOutline, label: 'common.tab.post', link: '/posts' },
  { icon: mdiFlagOutline, label: 'common.tab.game', link: '/games' },
  { icon: mdiAccountGroupOutline, label: 'common.tab.team', link: '/teams' },
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
  (!item.requiresDonations || donationsEnabled)

export const isNavigationItemActive = (item: PrimaryNavigationItem, pathname: string) => {
  if (item.link === '/') return pathname === '/'
  if (item.link.startsWith('/admin')) return pathname.startsWith('/admin')
  return pathname.startsWith(item.link)
}
