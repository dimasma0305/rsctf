import { Text } from '@mantine/core'
import { mdiCommentTextOutline, mdiFlagOutline, mdiSwordCross } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, useParams } from 'react-router'
import { IconTabs } from '@Components/IconTabs'

/**
 * Sub-navigation shared by the three challenge-admin views that used to be
 * separate sidebar tabs — Challenges, challenge Reviews, and A&D · KotH Ops.
 * They now live under one "Challenges" sidebar entry and switch here, so the
 * game-admin sidebar stays short and everything challenge-related is one hop away.
 */
export const ChallengeConsoleTabs: FC = () => {
  const { id } = useParams()
  const { pathname } = useLocation()
  const { t } = useTranslation()

  const active = pathname.includes('/adops')
    ? 'adops'
    : pathname.includes('challengereviews')
      ? 'challengereviews'
      : 'challenges'

  const tabs = [
    { key: 'challenges', icon: mdiFlagOutline, label: t('admin.tab.games.challenges') },
    { key: 'challengereviews', icon: mdiCommentTextOutline, label: t('admin.title.challenge_reviews', 'Reviews') },
    { key: 'adops', icon: mdiSwordCross, label: t('admin.tab.games.ad_ops', 'A&D · KotH Ops') },
  ]

  return (
    <IconTabs
      mode="navigation"
      active={tabs.findIndex((s) => s.key === active)}
      tabs={tabs.map((s) => ({
        tabKey: s.key,
        to: `/admin/games/${id}/${s.key}`,
        icon: <Icon path={s.icon} size={1} />,
        label: (
          <Text size="sm" fw={500}>
            {s.label}
          </Text>
        ),
      }))}
    />
  )
}
