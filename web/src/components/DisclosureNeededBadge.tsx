import { Badge, useComputedColorScheme } from '@mantine/core'
import { mdiAlertOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'

/** Text-first marker for a solved challenge that still needs an AI chat disclosure. */
export const DisclosureNeededBadge: FC = () => {
  const { t } = useTranslation()
  // Orange stays below 4.5:1 on white at badge sizes; red.9 reaches 5.5:1.
  const color = useComputedColorScheme('dark') === 'dark' ? 'orange' : 'red.9'
  return (
    <Badge
      size="sm"
      variant="outline"
      color={color}
      tt="none"
      leftSection={<Icon path={mdiAlertOutline} size={0.55} aria-hidden="true" />}
      data-ai-chat-disclosure-needed
    >
      {t('game.ai_chat.disclosure_needed', 'Disclosure needed')}
    </Badge>
  )
}
