import {
  ActionIcon,
  Alert,
  Badge,
  Button,
  Divider,
  Group,
  Paper,
  Stack,
  Text,
  TextInput,
  Title,
  Tooltip,
  useComputedColorScheme,
} from '@mantine/core'
import { useClipboard } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import {
  mdiAlertCircleOutline,
  mdiCheck,
  mdiChevronDown,
  mdiChevronUp,
  mdiContentCopy,
  mdiDeleteOutline,
  mdiLockOutline,
  mdiOpenInNew,
  mdiPlus,
  mdiRobotOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, FormEvent, useId, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { aiChatHostname, matchAiChatProvider, normalizeAiChatUrl, safeAiChatHref } from '@Utils/AiChatLinks'
import { httpErrorStatus } from '@Utils/HttpError'
import { showErrorMsg } from '@Utils/Shared'
import api from '@Api'

export interface AiChatLinksSectionProps {
  gameId: number
  challengeId: number
}

/** One non-polled read per mount; saves reconcile through the same cache entry. */
const AI_CHAT_READ_CONFIG = {
  refreshInterval: 0,
  revalidateOnFocus: false,
  revalidateOnReconnect: false,
  shouldRetryOnError: false,
}

type DraftStatus = { kind: 'idle' } | { kind: 'ok'; url: string; provider: string } | { kind: 'error'; message: string }

const sameLinks = (a: readonly string[], b: readonly string[]) =>
  a.length === b.length && a.every((url, index) => url === b[index])

export const AiChatLinksSection: FC<AiChatLinksSectionProps> = ({ gameId, challengeId }) => {
  const { t } = useTranslation()
  // red.6 is 3.8:1 on white; keep the verdict at WCAG AA in both schemes.
  const dark = useComputedColorScheme('dark') === 'dark'
  const errorColor = dark ? 'red.4' : 'red.8'
  // Orange stays below 4.5:1 on white at badge sizes; red.9 reaches 5.5:1.
  const blockedColor = dark ? 'orange' : 'red.9'
  const headingId = useId()
  const panelId = useId()
  const clipboard = useClipboard({ timeout: 1500 })

  const {
    data: state,
    error,
    isLoading,
    mutate,
  } = api.game.useGameGetAiChatLinks(gameId, challengeId, AI_CHAT_READ_CONFIG, gameId > 0 && challengeId > 0)

  const [expanded, setExpanded] = useState(false)
  const [input, setInput] = useState('')
  const [draft, setDraft] = useState<string[] | null>(null)
  const [saving, setSaving] = useState(false)

  const savedLinks = useMemo(() => state?.links.map((link) => link.url) ?? [], [state?.links])
  const links = draft ?? savedLinks
  const dirty = draft !== null && !sameLinks(draft, savedLinks)
  const editable = Boolean(state?.editable)
  const maxLinks = state?.maxLinks ?? 0
  const providers = state?.providers ?? []

  const reasonMessage = (reason: string, count = maxLinks) => {
    switch (reason) {
      case 'invalid':
        return t('challenge.ai_chat.reason.invalid', 'Enter a full link that starts with https://')
      case 'https_required':
        return t('challenge.ai_chat.reason.https_required', 'Only https:// links are accepted')
      case 'credentials':
        return t('challenge.ai_chat.reason.credentials', 'Links that contain a username or password are not accepted')
      case 'port':
        return t('challenge.ai_chat.reason.port', 'Links with a custom port are not accepted')
      case 'too_long':
        return t('challenge.ai_chat.reason.too_long', 'This link is too long')
      case 'unsupported':
        return t('challenge.ai_chat.reason.unsupported', 'This AI provider is not accepted')
      case 'duplicate':
        return t('challenge.ai_chat.reason.duplicate', 'This link is already in the list')
      default:
        return t('challenge.ai_chat.reason.full', 'You can attach up to {{count}} links', { count })
    }
  }

  const evaluateInput = (): DraftStatus => {
    const normalized = normalizeAiChatUrl(input)
    if (!normalized.ok) {
      return normalized.reason === 'empty'
        ? { kind: 'idle' }
        : { kind: 'error', message: reasonMessage(normalized.reason) }
    }
    const provider = matchAiChatProvider(normalized.url, providers)
    if (!provider) return { kind: 'error', message: reasonMessage('unsupported') }
    if (links.includes(normalized.url)) return { kind: 'error', message: reasonMessage('duplicate') }
    if (links.length >= maxLinks) return { kind: 'error', message: reasonMessage('full') }
    return { kind: 'ok', url: normalized.url, provider: provider.label }
  }
  const status = evaluateInput()

  // A disabled event (404) or a non-participant (400) has nothing to show here.
  const hiddenStatus = httpErrorStatus(error)
  if (hiddenStatus === 404 || hiddenStatus === 400 || hiddenStatus === 403) return null

  const providerFor = (url: string) => {
    const saved = state?.links.find((link) => link.url === url)
    const current = matchAiChatProvider(url, providers)
    return { label: current?.label ?? saved?.providerLabel ?? '', accepted: current !== null }
  }

  const onAdd = (event: FormEvent) => {
    event.preventDefault()
    if (status.kind !== 'ok' || !editable) return
    setDraft([...links, status.url])
    setInput('')
  }

  const onRemove = (url: string) => setDraft(links.filter((item) => item !== url))

  const onCopy = (url: string) => {
    clipboard.copy(url)
    showNotification({
      color: 'teal',
      message: t('challenge.ai_chat.copied', 'Link copied'),
      icon: <Icon path={mdiCheck} size={1} />,
    })
  }

  const onSave = async () => {
    if (!state || saving || !dirty) return
    setSaving(true)
    try {
      const { data } = await api.game.gameSaveAiChatLinks(gameId, challengeId, {
        links,
        expectedRevision: state.revision,
      })
      await mutate(data, { revalidate: false })
      setDraft(null)
      showNotification({
        color: 'teal',
        message: t('challenge.ai_chat.saved', 'AI chat links saved'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (saveError) {
      if (httpErrorStatus(saveError) === 409) {
        await mutate().catch(() => undefined)
        setDraft(null)
        showNotification({
          color: 'orange',
          title: t('challenge.ai_chat.conflict.title', 'Links changed'),
          message: t(
            'challenge.ai_chat.conflict.message',
            'A teammate changed these links. The latest list is shown; add your changes again.'
          ),
          icon: <Icon path={mdiAlertCircleOutline} size={1} />,
        })
      } else {
        showErrorMsg(saveError, t)
      }
    } finally {
      setSaving(false)
    }
  }

  const summary = !state
    ? isLoading
      ? t('challenge.ai_chat.loading', 'Loading…')
      : t('challenge.ai_chat.load_failed', 'AI chat links could not be loaded.')
    : t('challenge.ai_chat.count', '{{count}} of {{max}} links', { count: links.length, max: maxLinks })

  return (
    <Stack gap="xs" component="section" aria-labelledby={headingId} data-ai-chat-links>
      <Divider />
      <Group justify="space-between" gap="xs" wrap="wrap">
        <Group gap={6} wrap="nowrap" style={{ minWidth: 0 }}>
          <Icon path={mdiRobotOutline} size={0.8} aria-hidden="true" />
          <Title order={3} size="h5" id={headingId}>
            {t('challenge.ai_chat.title', 'AI chat links')}
          </Title>
          <Text size="xs" c="dimmed">
            {summary}
          </Text>
        </Group>
        <Button
          size="compact-sm"
          variant="subtle"
          aria-expanded={expanded}
          aria-controls={panelId}
          disabled={!state}
          rightSection={<Icon path={expanded ? mdiChevronUp : mdiChevronDown} size={0.7} aria-hidden="true" />}
          onClick={() => setExpanded((value) => !value)}
        >
          {expanded
            ? t('challenge.ai_chat.hide', 'Hide')
            : editable
              ? t('challenge.ai_chat.manage', 'Manage')
              : t('challenge.ai_chat.show', 'Show')}
        </Button>
      </Group>
      {!state && !isLoading && (
        <Group gap="xs">
          <Button size="compact-xs" variant="default" onClick={() => void mutate()}>
            {t('common.button.retry', 'Retry')}
          </Button>
        </Group>
      )}
      {state && expanded && (
        <Stack gap="xs" id={panelId}>
          <Text size="xs" c="dimmed">
            {t(
              'challenge.ai_chat.description',
              'Share the public links of AI chats your team used for this challenge. Organizers may review them.'
            )}
          </Text>
          {!editable && (
            <Alert color="gray" p="xs" icon={<Icon path={mdiLockOutline} size={0.8} />}>
              <Text size="xs">
                {state.solved
                  ? t('challenge.ai_chat.closed', 'The window for changing AI chat links has closed.')
                  : t('challenge.ai_chat.unsolved', 'Solve this challenge to attach AI chat links.')}
              </Text>
            </Alert>
          )}
          {editable && (
            <form onSubmit={onAdd}>
              <TextInput
                label={t('challenge.ai_chat.input_label', 'Share link')}
                placeholder="https://claude.ai/share/…"
                value={input}
                onChange={(event) => setInput(event.currentTarget.value)}
                disabled={providers.length === 0}
                inputMode="url"
                autoComplete="off"
                spellCheck={false}
                // Boolean error: Mantine sets aria-invalid without a second message.
                error={status.kind === 'error'}
                // The description slot is Mantine's aria-describedby target, so
                // the live verdict is both announced and tied to the field.
                inputWrapperOrder={['label', 'input', 'description']}
                description={
                  <span role="status" aria-live="polite" data-ai-chat-status>
                    {status.kind === 'ok' && (
                      <Badge component="span" color="teal" variant="light" size="sm" tt="none">
                        {t('challenge.ai_chat.matched', 'Matches {{provider}}', { provider: status.provider })}
                      </Badge>
                    )}
                    {status.kind === 'error' && (
                      <Text span size="xs" c={errorColor} fw={500}>
                        {status.message}
                      </Text>
                    )}
                  </span>
                }
                inputContainer={(field) => (
                  <Group gap="xs" wrap="nowrap" align="center">
                    <div style={{ flex: 1, minWidth: 0 }}>{field}</div>
                    <Button
                      type="submit"
                      variant="light"
                      disabled={status.kind !== 'ok'}
                      leftSection={<Icon path={mdiPlus} size={0.75} aria-hidden="true" />}
                    >
                      {t('challenge.ai_chat.add', 'Add')}
                    </Button>
                  </Group>
                )}
                styles={{ input: { fontFamily: 'monospace' } }}
              />
            </form>
          )}
          <Text size="xs" c="dimmed">
            {providers.length > 0
              ? t('challenge.ai_chat.supported', 'Supported: {{providers}}', {
                  providers: providers.map((provider) => provider.label).join(', '),
                })
              : t('challenge.ai_chat.no_providers', 'No AI providers are accepted right now.')}
          </Text>
          {links.length === 0 ? (
            <Text size="sm" c="dimmed">
              {t('challenge.ai_chat.empty', 'No links attached yet.')}
            </Text>
          ) : (
            <Stack gap={6} component="ul" m={0} p={0} style={{ listStyle: 'none' }}>
              {links.map((url) => {
                const provider = providerFor(url)
                const href = safeAiChatHref(url)
                const pending = !savedLinks.includes(url)
                return (
                  <Paper key={url} component="li" withBorder p="xs" radius="sm">
                    <Group justify="space-between" gap="xs" wrap="nowrap" align="flex-start">
                      <Stack gap={2} style={{ minWidth: 0, flex: 1 }}>
                        <Group gap={6} wrap="wrap">
                          <Badge size="sm" variant="light" tt="none">
                            {provider.label || t('challenge.ai_chat.unknown_provider', 'Unknown provider')}
                          </Badge>
                          {!provider.accepted && (
                            <Badge size="sm" variant="outline" color={blockedColor} tt="none">
                              {t('challenge.ai_chat.provider_blocked', 'Provider no longer accepted')}
                            </Badge>
                          )}
                          {pending && (
                            <Badge size="sm" variant="outline" color="gray" tt="none">
                              {t('challenge.ai_chat.unsaved_link', 'Not saved')}
                            </Badge>
                          )}
                        </Group>
                        <Text size="sm" fw={600} style={{ overflowWrap: 'anywhere' }}>
                          {aiChatHostname(url)}
                        </Text>
                        <Text size="xs" c="dimmed" ff="monospace" lineClamp={2} style={{ overflowWrap: 'anywhere' }}>
                          {url}
                        </Text>
                      </Stack>
                      <Group gap={2} wrap="nowrap">
                        <Tooltip label={t('challenge.ai_chat.copy', 'Copy link')}>
                          <ActionIcon
                            variant="subtle"
                            color="gray"
                            size="lg"
                            aria-label={t('challenge.ai_chat.copy', 'Copy link')}
                            onClick={() => onCopy(url)}
                          >
                            <Icon path={mdiContentCopy} size={0.75} aria-hidden="true" />
                          </ActionIcon>
                        </Tooltip>
                        {href && (
                          <Tooltip label={t('challenge.ai_chat.open', 'Open link in a new tab')}>
                            <ActionIcon
                              component="a"
                              href={href}
                              target="_blank"
                              rel="noopener noreferrer nofollow"
                              variant="subtle"
                              color="gray"
                              size="lg"
                              aria-label={t('challenge.ai_chat.open', 'Open link in a new tab')}
                            >
                              <Icon path={mdiOpenInNew} size={0.75} aria-hidden="true" />
                            </ActionIcon>
                          </Tooltip>
                        )}
                        {editable && (
                          <Tooltip label={t('challenge.ai_chat.remove', 'Remove link')}>
                            <ActionIcon
                              variant="subtle"
                              color="red"
                              size="lg"
                              aria-label={t('challenge.ai_chat.remove', 'Remove link')}
                              onClick={() => onRemove(url)}
                            >
                              <Icon path={mdiDeleteOutline} size={0.75} aria-hidden="true" />
                            </ActionIcon>
                          </Tooltip>
                        )}
                      </Group>
                    </Group>
                  </Paper>
                )
              })}
            </Stack>
          )}
          {editable && (
            <Group justify="space-between" gap="xs" wrap="wrap">
              <Text size="xs" c={dirty ? 'orange' : 'dimmed'} role="status" aria-live="polite">
                {dirty
                  ? t('challenge.ai_chat.unsaved', 'Unsaved changes')
                  : state.submittedBy
                    ? t('challenge.ai_chat.last_saved_by', 'Last saved by {{user}}', { user: state.submittedBy })
                    : ''}
              </Text>
              <Group gap="xs">
                {dirty && (
                  <Button variant="default" size="compact-sm" disabled={saving} onClick={() => setDraft(null)}>
                    {t('challenge.ai_chat.discard', 'Discard')}
                  </Button>
                )}
                <Button size="compact-sm" loading={saving} disabled={!dirty} onClick={() => void onSave()}>
                  {t('challenge.ai_chat.save', 'Save links')}
                </Button>
              </Group>
            </Group>
          )}
        </Stack>
      )}
    </Stack>
  )
}
