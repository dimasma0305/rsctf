import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import classes from './CompetitionNotice.module.css'

/** A visible summary with keyboard/touch-accessible meaning and next steps. */
export function CompetitionNotice({ title, children }: { title: string; children: ReactNode }) {
  const { t } = useTranslation()
  return (
    <details className={classes.notice} data-competition-notice>
      <summary>
        <span>{title}</span> <span className={classes.hint}>{t('game.arena.notice_details', 'Details')}</span>
      </summary>
      <div className={classes.content}>{children}</div>
    </details>
  )
}
