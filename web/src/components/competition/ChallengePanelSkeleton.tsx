import { Skeleton } from '@mantine/core'
import { useTranslation } from 'react-i18next'
import classes from '@Styles/ChallengePanel.module.css'
import type { ChallengeView } from './model'

/** Match the current explorer, including the saved view, during its first read. */
export function ChallengePanelSkeleton({ view }: { view: ChallengeView }) {
  const { t } = useTranslation()
  return (
    <div className={classes.explorer} role="status" aria-label={t('common.content.loading')} data-challenge-loading>
      <div className={classes.toolbar} aria-hidden="true">
        <div className={classes.toolbarMain}>
          <div className={classes.search}>
            <Skeleton height={16} width="8rem" mb={4} animate={false} />
            <Skeleton height={36} animate={false} />
          </div>
          <div className={classes.desktopSort}>
            <Skeleton height={16} width="5rem" mb={4} animate={false} />
            <Skeleton height={36} width="10rem" animate={false} />
          </div>
          <div className={classes.desktopView}>
            <Skeleton height={36} width="12rem" animate={false} />
          </div>
          <div className={classes.filterButton}>
            <Skeleton height={36} width="6.5rem" animate={false} />
          </div>
        </div>
        <div className={classes.toolbarSecondary}>
          <Skeleton height={46} width="8rem" animate={false} />
        </div>
      </div>
      <div className={view === 'cards' ? classes.challengeGrid : classes.loadingRows} aria-hidden="true">
        {Array.from({ length: view === 'globe' ? 1 : 6 }, (_, index) => (
          <Skeleton key={index} height={view === 'globe' ? 480 : view === 'cards' ? 160 : 64} animate={false} />
        ))}
      </div>
    </div>
  )
}
