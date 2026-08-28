import {
  mdiFileDocumentCheckOutline,
  mdiImageMultipleOutline,
  mdiShieldLockOutline,
  mdiSwordCross,
  mdiTextBoxOutline,
} from '@mdi/js'
import type { TFunction } from 'i18next'

export const gameInfoSections = (t: TFunction) => [
  { key: 'general', icon: mdiTextBoxOutline, label: t('admin.content.games.info.section.general', 'General') },
  {
    key: 'writeups',
    icon: mdiFileDocumentCheckOutline,
    label: t('admin.content.games.info.section.writeups', 'Summary & writeups'),
  },
  {
    key: 'ad',
    icon: mdiSwordCross,
    label: t('admin.content.games.info.section.ad', 'Attack & Defense · King of the Hill'),
  },
  { key: 'security', icon: mdiShieldLockOutline, label: t('admin.event_security.section', 'Event security') },
  {
    key: 'content',
    icon: mdiImageMultipleOutline,
    label: t('admin.content.games.info.section.content', 'Description & media'),
  },
]
