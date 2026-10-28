import { mdiChevronDown, mdiInformationOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import classes from './CompetitionNotice.module.css'

/** A visible summary with keyboard/touch-accessible meaning and next steps. */
export function CompetitionNotice({
  title,
  children,
  icon = mdiInformationOutline,
}: {
  title: string
  children: ReactNode
  icon?: string
}) {
  const { t } = useTranslation()
  return (
    <details className={classes.notice} data-competition-notice>
      <summary>
        <Icon path={icon} size={0.85} className={classes.icon} aria-hidden="true" />
        <span className={classes.title}>{title}</span>
        <span className={classes.hint}>
          {t('game.arena.notice_details', 'Details')}
          <Icon path={mdiChevronDown} size={0.7} className={classes.chevron} aria-hidden="true" />
        </span>
      </summary>
      <div className={classes.content}>{children}</div>
    </details>
  )
}
