import { Alert, Badge, Code, Group, Loader, Modal, Paper, Stack, Text, useComputedColorScheme } from '@mantine/core'
import { mdiAlertCircleOutline, mdiMinus, mdiPlus } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import type { TFunction } from 'i18next'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { aiChatDelay } from '@Utils/AiChatLinks'
import { useLanguage } from '@Utils/I18n'
import { tryGetErrorMsg } from '@Utils/Shared'
import api, { type AiChatLinkEvent, type AiChatLinkRecord } from '@Api'

/** "4 min after solve": whole units, count-neutral abbreviations. */
export const aiChatDelayLabel = (t: TFunction, seconds: number) => {
  const { unit, count } = aiChatDelay(seconds)
  switch (unit) {
    case 'seconds':
      return t('game.ai_chat.delay.seconds', '{{count}} s after solve', { count })
    case 'minutes':
      return t('game.ai_chat.delay.minutes', '{{count}} min after solve', { count })
    case 'hours':
      return t('game.ai_chat.delay.hours', '{{count}} h after solve', { count })
    default:
      return t('game.ai_chat.delay.days', '{{count}} d after solve', { count })
  }
}

export interface AiChatHistoryModalProps {
  gameId: number
  /** The record whose history is shown; null keeps the dialog closed. */
  record: AiChatLinkRecord | null
  onClose: () => void
}

export const AiChatHistoryModal: FC<AiChatHistoryModalProps> = ({ gameId, record, onClose }) => {
  const { t } = useTranslation()
  const { locale } = useLanguage()
  const dark = useComputedColorScheme('dark') === 'dark'
  // Orange stays below 4.5:1 on white at badge sizes; red.9 reaches 5.5:1.
  const removedColor = dark ? 'orange' : 'red.9'
  const addedColor = dark ? 'teal' : 'teal.9'

  const { data, error, isLoading } = api.game.useGameGetAiChatLinkEvents(
    gameId,
    record?.participationId ?? 0,
    record?.challengeId ?? 0,
    { refreshInterval: 0, revalidateOnFocus: false, shouldRetryOnError: false },
    record !== null && gameId > 0
  )

  const actionLabel = (action: AiChatLinkEvent['action']) =>
    action === 'Created'
      ? t('game.ai_chat.history.action.created', 'Created')
      : action === 'Edited'
        ? t('game.ai_chat.history.action.edited', 'Edited')
        : t('game.ai_chat.history.action.cleared', 'Cleared')

  const time = (value: number) => (
    <time dateTime={new Date(value).toISOString()}>{dayjs(value).locale(locale).format('SL LTS')}</time>
  )

  const linkChange = (url: string, kind: 'added' | 'removed') => (
    <Group key={`${kind}:${url}`} component="li" gap={6} wrap="nowrap" align="flex-start">
      <Badge
        size="sm"
        variant="outline"
        color={kind === 'added' ? addedColor : removedColor}
        tt="none"
        style={{ flexShrink: 0 }}
        leftSection={<Icon path={kind === 'added' ? mdiPlus : mdiMinus} size={0.55} aria-hidden="true" />}
      >
        {kind === 'added' ? t('game.ai_chat.history.added', 'Added') : t('game.ai_chat.history.removed', 'Removed')}
      </Badge>
      <Text
        size="xs"
        ff="monospace"
        td={kind === 'removed' ? 'line-through' : undefined}
        style={{ overflowWrap: 'anywhere', minWidth: 0 }}
      >
        {url}
      </Text>
    </Group>
  )

  const eventItem = (event: AiChatLinkEvent) => {
    const declarationChanged = event.previousDeclaredNoAi !== event.declaredNoAi
    return (
      <Paper key={event.id} component="li" withBorder p="xs" radius="sm" data-ai-chat-event={event.action}>
        <Stack gap={6}>
          <Group gap="xs" wrap="wrap">
            <Badge size="sm" variant="light" color="gray" tt="none">
              {actionLabel(event.action)}
            </Badge>
            <Text size="xs" c="dimmed">
              {time(event.occurredAt)}
            </Text>
            {event.secondsSinceSolve !== null && (
              <Text size="xs" c="dimmed">
                {aiChatDelayLabel(t, event.secondsSinceSolve)}
              </Text>
            )}
            <Text size="xs" c="dimmed">
              {event.userName
                ? t('game.ai_chat.history.by', 'by {{user}}', { user: event.userName })
                : t('game.ai_chat.history.unknown_user', 'by an unknown user')}
            </Text>
            <Text size="xs" c="dimmed">
              {t('game.ai_chat.history.revision', 'revision {{revision}}', { revision: event.revision })}
            </Text>
          </Group>
          {declarationChanged && (
            <Text size="sm" fw={600}>
              {event.declaredNoAi
                ? t('game.ai_chat.history.declared_no_ai', 'Declared: no AI used')
                : t('game.ai_chat.history.withdrew_no_ai', 'Withdrew the "No AI used" declaration')}
            </Text>
          )}
          {(event.added.length > 0 || event.removed.length > 0) && (
            <Stack gap={4} component="ul" m={0} p={0} style={{ listStyle: 'none' }}>
              {event.added.map((url) => linkChange(url, 'added'))}
              {event.removed.map((url) => linkChange(url, 'removed'))}
            </Stack>
          )}
          {event.networkHint && (
            <Text size="xs" c="dimmed">
              {t('game.ai_chat.history.network', 'Network')} <Code>{event.networkHint}</Code>
            </Text>
          )}
        </Stack>
      </Paper>
    )
  }

  return (
    <Modal
      opened={record !== null}
      onClose={onClose}
      size="min(40rem, calc(100vw - 1.5rem))"
      title={t('game.ai_chat.history.title', 'Disclosure history')}
      closeButtonProps={{ 'aria-label': t('common.button.close', 'Close') }}
      data-ai-chat-history
    >
      {record && (
        <Stack gap="sm">
          <Text size="sm" style={{ overflowWrap: 'anywhere' }}>
            {record.teamName} · {record.challengeTitle}
            {record.solvedAt !== null && (
              <Text span size="xs" c="dimmed">
                {' · '}
                {t('game.ai_chat.solved_at', 'Solved {{time}}', {
                  time: dayjs(record.solvedAt).locale(locale).format('SL LTS'),
                })}
              </Text>
            )}
          </Text>
          {isLoading && (
            <Group gap="xs" role="status">
              <Loader size="xs" />
              <Text size="sm">{t('game.ai_chat.history.loading', 'Loading history…')}</Text>
            </Group>
          )}
          {error && (
            <Alert color="red" p="xs" icon={<Icon path={mdiAlertCircleOutline} size={0.8} aria-hidden="true" />}>
              <Text size="sm">{tryGetErrorMsg(error, t)}</Text>
            </Alert>
          )}
          {data && data.items.length === 0 && (
            <Text size="sm" c="dimmed">
              {t('game.ai_chat.history.empty', 'No changes were recorded.')}
            </Text>
          )}
          {data && data.items.length > 0 && (
            <Stack
              gap="xs"
              component="ol"
              m={0}
              p={0}
              style={{ listStyle: 'none' }}
              aria-label={t('game.ai_chat.history.list_label', 'Disclosure changes, oldest first')}
            >
              {data.items.map(eventItem)}
            </Stack>
          )}
          {data?.truncated && (
            <Text size="xs" c="dimmed">
              {t('game.ai_chat.history.truncated', 'Only the first 200 changes are shown.')}
            </Text>
          )}
        </Stack>
      )}
    </Modal>
  )
}
