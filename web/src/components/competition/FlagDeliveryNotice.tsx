import { Button } from '@mantine/core'
import { useTranslation } from 'react-i18next'
import { CompetitionNotice } from './CompetitionNotice'

export function FlagDeliveryNotice({
  count,
  round,
  onToolkit,
}: {
  count: number
  round: number
  onToolkit?: () => void
}) {
  const { t } = useTranslation()
  if (count <= 0 || round <= 0) return null
  return (
    <CompetitionNotice
      title={t('game.content.ad.delivery_notice.title', {
        count,
        defaultValue: '{{count}} A&D flag deliveries failed across this event',
      })}
    >
      <p>
        {t('game.content.ad.delivery_notice.meaning', {
          round,
          defaultValue:
            'In round {{round}}, RSCTF could not confirm that the new defending flag reached some team services. This is an event-wide count, not a count of failed attacks or necessarily a problem with your team.',
        })}
      </p>
      <p>
        {t(
          'game.content.ad.delivery_notice.impact',
          'An affected service may still contain an older flag. This notice alone does not identify the affected teams or establish a scoring penalty.'
        )}
      </p>
      <p>
        {t(
          'game.content.ad.delivery_notice.action',
          'Open your A&D challenge to check your service and connection status. If something is wrong, contact the event organizer and include the round and challenge name. Organizers should inspect A&D operations and flag-delivery logs before changing or restarting services.'
        )}
      </p>
      {onToolkit && (
        <Button variant="default" size="xs" onClick={onToolkit}>
          {t('game.button.ad.open_toolkit', 'A&D Toolkit')}
        </Button>
      )}
    </CompetitionNotice>
  )
}
