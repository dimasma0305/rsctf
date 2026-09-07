import { Text } from '@mantine/core'
import { useTranslation } from 'react-i18next'
import classes from '@Styles/PlayerGuide.module.css'

// Tour and contextual tips share one readable instruction, with optional detail
// disclosed on demand. The active controls stay mounted when a step changes.
export const GuideStepContent = ({
  stepId,
  body,
  note,
  command,
}: {
  stepId: string
  body: string
  note?: string
  command?: string
}) => {
  const { t } = useTranslation()
  return (
    <div className={classes.stepContent} data-guide-step-content>
      <div role="status" aria-live="polite" aria-atomic="true">
        <Text key={`${stepId}:body`} size="sm" className={classes.stepCopy}>
          {body}
        </Text>
      </div>
      {command && (
        <Text component="code" size="sm" className={classes.command}>
          {command}
        </Text>
      )}
      {note && (
        <details key={stepId} className={classes.stepDetails} data-guide-step-details>
          <summary>{t('guide.tour.more_detail', 'More detail')}</summary>
          <Text size="sm" c="dimmed" className={classes.note}>
            {note}
          </Text>
        </details>
      )}
    </div>
  )
}
