import { Alert, Button, Group, Stack, Text } from '@mantine/core'
import { mdiAlertOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import { aiChatPendingChallenges } from '@Utils/AiChatLinks'
import api, { type ChallengeInfo } from '@Api'

/** Fetched once per event page; solves and disclosure saves refresh it explicitly. */
const AI_CHAT_PENDING_READ_CONFIG = {
  refreshInterval: 0,
  revalidateOnFocus: false,
  revalidateOnReconnect: false,
  shouldRetryOnError: false,
}

/**
 * The caller team's solved challenges that still need an AI chat disclosure,
 * in catalog order. Only read when the event requires disclosures; a missing
 * or failed read yields an empty list so nothing is ever blocked on it.
 */
export const useAiChatPendingChallenges = (gameId: number, required: boolean, catalog: readonly ChallengeInfo[]) => {
  const { data } = api.game.useGameGetAiChatPending(gameId, AI_CHAT_PENDING_READ_CONFIG, required && gameId > 0)
  return useMemo(() => aiChatPendingChallenges(required ? data : undefined, catalog), [catalog, data, required])
}

export interface AiChatPendingNoticeProps {
  challenges: readonly ChallengeInfo[]
  onOpen: (challenge: ChallengeInfo) => void
}

export const AiChatPendingNotice: FC<AiChatPendingNoticeProps> = ({ challenges, onOpen }) => {
  const { t } = useTranslation()
  if (challenges.length === 0) return null

  return (
    <Alert
      role="status"
      color="orange"
      p="sm"
      icon={<Icon path={mdiAlertOutline} size={0.9} aria-hidden="true" />}
      data-ai-chat-pending-banner
    >
      <Stack gap="xs">
        <Stack gap={2}>
          <Text size="sm" fw={700}>
            {t('game.ai_chat.pending_banner.title', '{{count}} solved challenges need an AI chat disclosure', {
              count: challenges.length,
            })}
          </Text>
          <Text size="xs">
            {t(
              'game.ai_chat.pending_banner.description',
              'This event requires a disclosure after every solve. Open a challenge to add the AI chat links your team used, or declare that no AI was used.'
            )}
          </Text>
        </Stack>
        <Group gap="xs" wrap="wrap">
          {challenges.map((challenge) => (
            <Button
              key={challenge.id}
              size="compact-sm"
              variant="default"
              aria-label={t('game.ai_chat.pending_banner.open', 'Add disclosure: {{title}}', {
                title: challenge.title,
              })}
              aria-haspopup="dialog"
              onClick={() => onOpen(challenge)}
            >
              {challenge.title}
            </Button>
          ))}
        </Group>
      </Stack>
    </Alert>
  )
}
