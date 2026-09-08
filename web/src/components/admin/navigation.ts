import {
  mdiAccountGroupOutline,
  mdiAccountKey,
  mdiBullhornOutline,
  mdiClockOutline,
  mdiCommentTextOutline,
  mdiFileDocumentCheckOutline,
  mdiFlagOutline,
  mdiFlagVariantOutline,
  mdiSwordCross,
  mdiTagOutline,
  mdiTextBoxOutline,
  mdiViewDashboardOutline,
  mdiCubeOutline,
  mdiEmailOutline,
  mdiShieldCheckOutline,
  mdiKeyChainVariant,
  mdiPackageVariantClosed,
  mdiHammerWrench,
  mdiHandHeart,
  mdiHeartPulse,
} from '@mdi/js'
import { Role, type ProfileUserInfoModel } from '@Api'

// Shared navigation hints only; each event and operation remains server-authorized.
export const EVENT_ADMIN_SECTIONS = [
  { path: 'info', label: 'admin.tab.games.info', fallback: 'Information', icon: mdiTextBoxOutline, group: 'setup' },
  { path: 'divisions', label: 'admin.tab.games.divisions', fallback: 'Divisions', icon: mdiTagOutline, group: 'setup' },
  {
    path: 'managers',
    label: 'admin.tab.games.managers',
    fallback: 'Managers',
    icon: mdiAccountKey,
    group: 'setup',
    adminOnly: true,
  },
  {
    path: 'challenges',
    label: 'admin.tab.games.challenges',
    fallback: 'Challenges',
    icon: mdiFlagOutline,
    group: 'competition',
  },
  {
    path: 'adops',
    label: 'admin.tab.games.ad_ops',
    fallback: 'A&D · KotH Ops',
    icon: mdiSwordCross,
    group: 'competition',
  },
  {
    path: 'pending',
    label: 'admin.tab.games.pending',
    fallback: 'Pending',
    icon: mdiClockOutline,
    group: 'competition',
  },
  {
    path: 'challengereviews',
    label: 'admin.navigation.challenge_reviews',
    fallback: 'Challenge reviews',
    icon: mdiCommentTextOutline,
    group: 'competition',
  },
  {
    path: 'flagegress',
    label: 'admin.tab.games.flag_egress',
    fallback: 'Flag egress',
    icon: mdiFlagVariantOutline,
    group: 'competition',
  },
  { path: 'review', label: 'admin.tab.games.review', fallback: 'Teams', icon: mdiAccountGroupOutline, group: 'people' },
  {
    path: 'writeups',
    label: 'admin.tab.games.writeups',
    fallback: 'Writeups',
    icon: mdiFileDocumentCheckOutline,
    group: 'people',
  },
  { path: 'notices', label: 'admin.tab.games.notices', fallback: 'Notices', icon: mdiBullhornOutline, group: 'people' },
] as const

export const getEventAdminSections = (user?: ProfileUserInfoModel) =>
  !user || (user.role !== Role.Admin && !user.hasManagedGames)
    ? []
    : EVENT_ADMIN_SECTIONS.filter((item) => !('adminOnly' in item) || user.role === Role.Admin)

export const getAdminEventContext = (pathname: string) => {
  const match = pathname.match(/^\/admin\/games\/(\d+)(?:\/([^/]+))?(?:\/([^/]+))?(?:\/([^/]+))?\/?$/)
  if (!match) return null
  const [, id, section = 'info', child, detail] = match
  const sectionPath = section === 'challenges' && child === 'pending' ? 'pending' : section
  const current = EVENT_ADMIN_SECTIONS.find((item) => item.path === sectionPath)
  const challengeId = section === 'challenges' && child && /^\d+$/.test(child) ? child : undefined
  return { id, section: current, challengeId, flags: Boolean(challengeId && detail === 'flags') }
}

export const SETTINGS_SECTIONS = [
  { key: 'platform', icon: mdiViewDashboardOutline, keywords: 'branding logo title theme' },
  { key: 'account', icon: mdiAccountGroupOutline, keywords: 'users teams registration password' },
  { key: 'container', icon: mdiCubeOutline, keywords: 'docker network vpn lifetime' },
  { key: 'email', icon: mdiEmailOutline, keywords: 'mail smtp credentials' },
  { key: 'captcha', icon: mdiShieldCheckOutline, keywords: 'turnstile verification' },
  { key: 'oauth', icon: mdiKeyChainVariant, keywords: 'login google discord' },
  { key: 'registry_pull', icon: mdiPackageVariantClosed, keywords: 'docker registry pull credentials' },
  { key: 'build_registry', icon: mdiHammerWrench, keywords: 'images build push registry' },
  { key: 'donations', icon: mdiHandHeart, keywords: 'payments' },
  { key: 'diagnostics', icon: mdiHeartPulse, keywords: 'health proxy ip trust' },
] as const

export type SettingsSectionKey = (typeof SETTINGS_SECTIONS)[number]['key']
export const getSettingsSection = (search: string): SettingsSectionKey => {
  const key = new URLSearchParams(search).get('section')
  return SETTINGS_SECTIONS.find((item) => item.key === key)?.key ?? 'platform'
}
