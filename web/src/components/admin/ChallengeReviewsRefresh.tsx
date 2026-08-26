import { ActionIcon } from '@mantine/core'
import { mdiRefresh } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC } from 'react'

interface ChallengeReviewsRefreshProps {
  label: string
  refreshReviews: () => unknown
  refreshAnalytics: () => unknown
}

/** Keeps both SWR mutations render-owned; event handlers must never call a hook. */
export const ChallengeReviewsRefresh: FC<ChallengeReviewsRefreshProps> = ({
  label,
  refreshReviews,
  refreshAnalytics,
}) => (
  <ActionIcon
    aria-label={label}
    onClick={() => {
      const reviews = refreshReviews()
      const analytics = refreshAnalytics()
      void Promise.allSettled([Promise.resolve(reviews), Promise.resolve(analytics)])
    }}
  >
    <Icon path={mdiRefresh} size={1} />
  </ActionIcon>
)
