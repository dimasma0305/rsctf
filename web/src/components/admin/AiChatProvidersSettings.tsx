import {
  Alert,
  Badge,
  Button,
  Code,
  Divider,
  Group,
  Paper,
  SimpleGrid,
  Stack,
  Switch,
  Text,
  TextInput,
  Textarea,
  Title,
  useComputedColorScheme,
} from '@mantine/core'
import { modals } from '@mantine/modals'
import { mdiAlertCircleOutline, mdiDeleteOutline, mdiPencilOutline, mdiPlus } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, FormEvent, useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import {
  AI_CHAT_PROVIDER_LABEL_MAX_LENGTH,
  AI_CHAT_PROVIDER_PATTERN_MAX_LENGTH,
  aiChatMatchOrder,
  matchAiChatProvider,
  normalizeAiChatUrl,
  validateAiChatPattern,
  validateAiChatProviderKey,
} from '@Utils/AiChatLinks'
import { showErrorMsg, showSuccessMsg, tryGetErrorMsg } from '@Utils/Shared'
import api, { AiChatProviderListModel, AiChatProviderModel } from '@Api'

/** Shared with the settings status map so both read one cache entry. */
export const AI_CHAT_PROVIDERS_READ_CONFIG = {
  refreshInterval: 0,
  revalidateOnFocus: false,
  revalidateOnReconnect: false,
}

export const useAiChatProviders = (enabled = true) =>
  api.admin.useAdminGetAiChatProviders(AI_CHAT_PROVIDERS_READ_CONFIG, enabled)

interface ProviderForm {
  /** Existing custom key when editing; null when creating. */
  editing: string | null
  key: string
  label: string
  pattern: string
}

const EMPTY_FORM: ProviderForm = { editing: null, key: '', label: '', pattern: '' }

export const AiChatProvidersSettings: FC = () => {
  const { t } = useTranslation()
  const dark = useComputedColorScheme('dark') === 'dark'
  const { data, error, mutate } = useAiChatProviders()
  const [pending, setPending] = useState<string | null>(null)
  const [form, setForm] = useState<ProviderForm>(EMPTY_FORM)
  const [formSubmitted, setFormSubmitted] = useState(false)
  const [formSaving, setFormSaving] = useState(false)
  const [testUrl, setTestUrl] = useState('')
  const formHeadingId = useId()

  const providers = data?.providers ?? []
  const builtins = providers.filter((provider) => provider.builtin)
  const custom = providers.filter((provider) => !provider.builtin)
  const maxCustom = data?.maxCustomProviders ?? 0
  const atCustomLimit = form.editing === null && custom.length >= maxCustom

  const replaceProvider = (list: AiChatProviderListModel, saved: AiChatProviderModel): AiChatProviderListModel => {
    const exists = list.providers.some((provider) => provider.key === saved.key)
    return {
      ...list,
      providers: exists
        ? list.providers.map((provider) => (provider.key === saved.key ? saved : provider))
        : [...list.providers, saved],
    }
  }

  const onToggle = async (provider: AiChatProviderModel, enabled: boolean) => {
    if (pending) return
    setPending(provider.key)
    try {
      // Built-in keys accept only `enabled`; custom providers keep their rule.
      const body = provider.builtin ? { enabled } : { enabled, label: provider.label, pattern: provider.pattern }
      const { data: saved } = await api.admin.adminSaveAiChatProvider(provider.key, body)
      await mutate((current) => (current ? replaceProvider(current, saved) : current), { revalidate: false })
    } catch (toggleError) {
      showErrorMsg(toggleError, t)
    } finally {
      setPending(null)
    }
  }

  const onDelete = (provider: AiChatProviderModel) =>
    modals.openConfirmModal({
      title: t('admin.content.settings.ai_links.delete_title', 'Delete {{provider}}?', { provider: provider.label }),
      children: (
        <Text size="sm">
          {t(
            'admin.content.settings.ai_links.delete_message',
            'Teams can no longer add links from this provider. Saved links stay visible to organizers, marked as blocked.'
          )}
        </Text>
      ),
      labels: {
        confirm: t('admin.content.settings.ai_links.delete', 'Delete'),
        cancel: t('common.modal.cancel', 'Cancel'),
      },
      confirmProps: { color: 'red' },
      onConfirm: async () => {
        setPending(provider.key)
        try {
          await api.admin.adminDeleteAiChatProvider(provider.key)
          await mutate(
            (current) =>
              current
                ? { ...current, providers: current.providers.filter((item) => item.key !== provider.key) }
                : current,
            { revalidate: false }
          )
          if (form.editing === provider.key) setForm(EMPTY_FORM)
          showSuccessMsg(t('admin.content.settings.ai_links.deleted', 'Provider deleted'))
        } catch (deleteError) {
          showErrorMsg(deleteError, t)
        } finally {
          setPending(null)
        }
      },
    })

  const keyReason = form.editing === null ? validateAiChatProviderKey(form.key, providers) : null
  const keyError =
    keyReason === 'format'
      ? t(
          'admin.content.settings.ai_links.key_format',
          'Use 1–40 lowercase letters, digits, or hyphens, starting with a letter or digit.'
        )
      : keyReason === 'builtin'
        ? t('admin.content.settings.ai_links.key_builtin', 'This key belongs to a built-in provider.')
        : keyReason === 'duplicate'
          ? t('admin.content.settings.ai_links.key_duplicate', 'A custom provider already uses this key.')
          : null
  const label = form.label.trim()
  const labelError =
    label.length === 0 || label.length > AI_CHAT_PROVIDER_LABEL_MAX_LENGTH
      ? t('admin.content.settings.ai_links.label_invalid', 'Enter a label of 1–64 characters.')
      : null
  const patternReason = validateAiChatPattern(form.pattern)
  const patternError =
    patternReason === 'https_prefix'
      ? t('admin.content.settings.ai_links.pattern_prefix', 'The pattern must start with https:// and an escaped host.')
      : patternReason === 'alternation'
        ? t(
            'admin.content.settings.ai_links.pattern_alternation',
            'Wrap alternatives in a group such as (?:a|b); a top-level | would match other sites.'
          )
        : patternReason === 'syntax'
        ? t('admin.content.settings.ai_links.pattern_syntax', 'The pattern is not a valid regular expression.')
        : patternReason
          ? t('admin.content.settings.ai_links.pattern_length', 'Enter a pattern of 1–512 characters.')
          : null
  const formValid = !keyError && !labelError && !patternError && !atCustomLimit

  const onSubmitForm = async (event: FormEvent) => {
    event.preventDefault()
    setFormSubmitted(true)
    if (!formValid || formSaving) return
    const key = form.editing ?? form.key
    const existing = providers.find((provider) => provider.key === key)
    setFormSaving(true)
    try {
      const { data: saved } = await api.admin.adminSaveAiChatProvider(key, {
        enabled: existing?.enabled ?? true,
        label,
        pattern: form.pattern,
      })
      await mutate((current) => (current ? replaceProvider(current, saved) : current), { revalidate: false })
      showSuccessMsg(
        form.editing
          ? t('admin.content.settings.ai_links.updated', 'Provider updated')
          : t('admin.content.settings.ai_links.created', 'Provider added')
      )
      setForm(EMPTY_FORM)
      setFormSubmitted(false)
    } catch (saveError) {
      showErrorMsg(saveError, t)
    } finally {
      setFormSaving(false)
    }
  }

  const testResult = (() => {
    if (!testUrl.trim()) return null
    const normalized = normalizeAiChatUrl(testUrl)
    if (!normalized.ok) {
      const reasons: Record<string, string> = {
        invalid: t('admin.content.settings.ai_links.test_reason.invalid', 'Not an absolute URL.'),
        https_required: t(
          'admin.content.settings.ai_links.test_reason.https_required',
          'Only https:// links are accepted.'
        ),
        credentials: t(
          'admin.content.settings.ai_links.test_reason.credentials',
          'Links with a username or password are rejected.'
        ),
        port: t('admin.content.settings.ai_links.test_reason.port', 'Links with a custom port are rejected.'),
        too_long: t('admin.content.settings.ai_links.test_reason.too_long', 'The link is longer than 2048 characters.'),
      }
      return { ok: false, text: reasons[normalized.reason] ?? reasons.invalid }
    }
    const provider = matchAiChatProvider(normalized.url, aiChatMatchOrder(providers))
    return provider
      ? {
          ok: true,
          text: t('admin.content.settings.ai_links.test_match', 'Matches {{provider}} ({{key}}): {{url}}', {
            provider: provider.label,
            key: provider.key,
            url: normalized.url,
          }),
        }
      : {
          ok: false,
          text: t('admin.content.settings.ai_links.test_no_match', 'No enabled provider matches {{url}}', {
            url: normalized.url,
          }),
        }
  })()

  const providerCard = (provider: AiChatProviderModel) => (
    <Paper key={provider.key} withBorder p="sm" radius="md" component="li">
      <Stack gap={6}>
        <Group justify="space-between" gap="xs" wrap="wrap">
          <Group gap={6} wrap="wrap" style={{ minWidth: 0 }}>
            <Text fw={600}>{provider.label}</Text>
            <Badge size="sm" variant="light" color="gray" tt="none" ff="monospace">
              {provider.key}
            </Badge>
          </Group>
          <Switch
            label={t('admin.content.settings.ai_links.enabled', 'Enabled')}
            aria-label={t('admin.content.settings.ai_links.enabled_for', 'Enabled: {{provider}}', {
              provider: provider.label,
            })}
            checked={provider.enabled}
            disabled={pending !== null}
            onChange={(event) => void onToggle(provider, event.currentTarget.checked)}
          />
        </Group>
        <Code block style={{ whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }}>
          {provider.pattern}
        </Code>
        {provider.examples.length > 0 && (
          <Stack gap={2}>
            <Text size="xs" c="dimmed">
              {t('admin.content.settings.ai_links.examples', 'Examples')}
            </Text>
            {provider.examples.map((example) => (
              <Text key={example} size="xs" ff="monospace" style={{ overflowWrap: 'anywhere' }}>
                {example}
              </Text>
            ))}
          </Stack>
        )}
        {!provider.builtin && (
          <Group gap="xs" justify="flex-end">
            <Button
              size="compact-sm"
              variant="default"
              disabled={pending !== null}
              leftSection={<Icon path={mdiPencilOutline} size={0.7} aria-hidden="true" />}
              aria-label={t('admin.content.settings.ai_links.edit_named', 'Edit {{provider}}', {
                provider: provider.label,
              })}
              onClick={() => {
                setForm({ editing: provider.key, key: provider.key, label: provider.label, pattern: provider.pattern })
                setFormSubmitted(false)
              }}
            >
              {t('admin.content.settings.ai_links.edit', 'Edit')}
            </Button>
            <Button
              size="compact-sm"
              variant="light"
              color="red"
              disabled={pending !== null}
              loading={pending === provider.key}
              leftSection={<Icon path={mdiDeleteOutline} size={0.7} aria-hidden="true" />}
              aria-label={t('admin.content.settings.ai_links.delete_named', 'Delete {{provider}}', {
                provider: provider.label,
              })}
              onClick={() => onDelete(provider)}
            >
              {t('admin.content.settings.ai_links.delete', 'Delete')}
            </Button>
          </Group>
        )}
      </Stack>
    </Paper>
  )

  const showFormErrors = formSubmitted || undefined

  return (
    <Stack gap="sm">
      <Title order={2}>{t('admin.content.settings.ai_links.title', 'AI chat links')}</Title>
      <Text size="sm" c="dimmed">
        {t(
          'admin.content.settings.ai_links.description',
          'Choose which AI chat providers teams may link from solved challenges. Events opt in from their Information page.'
        )}
      </Text>
      <Divider />
      {error && !data && (
        <Alert color="red" icon={<Icon path={mdiAlertCircleOutline} size={0.9} />}>
          <Group justify="space-between" gap="sm">
            <Text size="sm">{tryGetErrorMsg(error, t)}</Text>
            <Button size="compact-sm" variant="default" onClick={() => void mutate()}>
              {t('common.button.retry', 'Retry')}
            </Button>
          </Group>
        </Alert>
      )}

      <Title order={3} size="h4">
        {t('admin.content.settings.ai_links.builtin_title', 'Built-in providers')}
      </Title>
      <SimpleGrid
        type="container"
        cols={{ base: 1, '48em': 2 }}
        component="ul"
        m={0}
        p={0}
        style={{ listStyle: 'none' }}
      >
        {builtins.map(providerCard)}
      </SimpleGrid>

      <Group justify="space-between" gap="xs" mt="sm">
        <Title order={3} size="h4">
          {t('admin.content.settings.ai_links.custom_title', 'Custom providers')}
        </Title>
        <Text size="xs" c="dimmed">
          {t('admin.content.settings.ai_links.custom_count', '{{count}} of {{max}}', {
            count: custom.length,
            max: maxCustom,
          })}
        </Text>
      </Group>
      {custom.length === 0 ? (
        <Text size="sm" c="dimmed">
          {t('admin.content.settings.ai_links.custom_empty', 'No custom providers yet.')}
        </Text>
      ) : (
        <SimpleGrid
          type="container"
          cols={{ base: 1, '48em': 2 }}
          component="ul"
          m={0}
          p={0}
          style={{ listStyle: 'none' }}
        >
          {custom.map(providerCard)}
        </SimpleGrid>
      )}

      <Paper withBorder p="md" radius="md" component="section" aria-labelledby={formHeadingId}>
        <form onSubmit={(event) => void onSubmitForm(event)} noValidate>
          <Stack gap="sm">
            <Title order={3} size="h5" id={formHeadingId}>
              {form.editing
                ? t('admin.content.settings.ai_links.edit_title', 'Edit {{key}}', { key: form.editing })
                : t('admin.content.settings.ai_links.add_title', 'Add provider')}
            </Title>
            {atCustomLimit && (
              <Text size="sm" c="orange">
                {t('admin.content.settings.ai_links.custom_limit', 'The custom provider limit is reached.')}
              </Text>
            )}
            <SimpleGrid type="container" cols={{ base: 1, '32em': 2 }}>
              <TextInput
                label={t('admin.content.settings.ai_links.key', 'Key')}
                description={t('admin.content.settings.ai_links.key_description', 'Lowercase, e.g. copilot')}
                value={form.key}
                readOnly={form.editing !== null}
                required
                maxLength={40}
                autoComplete="off"
                spellCheck={false}
                styles={{ input: { fontFamily: 'monospace' } }}
                error={showFormErrors && keyError}
                onChange={(event) => setForm({ ...form, key: event.currentTarget.value })}
              />
              <TextInput
                label={t('admin.content.settings.ai_links.label', 'Label')}
                description={t('admin.content.settings.ai_links.label_description', 'Shown to players and organizers')}
                value={form.label}
                required
                maxLength={AI_CHAT_PROVIDER_LABEL_MAX_LENGTH}
                error={showFormErrors && labelError}
                onChange={(event) => setForm({ ...form, label: event.currentTarget.value })}
              />
            </SimpleGrid>
            <Textarea
              label={t('admin.content.settings.ai_links.pattern', 'URL pattern')}
              description={t(
                'admin.content.settings.ai_links.pattern_description',
                'Regular expression matched against the whole normalized link, e.g. https://copilot\\.microsoft\\.com/shares/[A-Za-z0-9]{8,64}'
              )}
              value={form.pattern}
              required
              autosize
              minRows={2}
              maxLength={AI_CHAT_PROVIDER_PATTERN_MAX_LENGTH}
              spellCheck={false}
              styles={{ input: { fontFamily: 'monospace' } }}
              error={(showFormErrors || form.pattern.length > 0) && patternError}
              onChange={(event) => setForm({ ...form, pattern: event.currentTarget.value })}
            />
            <Group justify="flex-end" gap="xs">
              {form.editing && (
                <Button
                  variant="default"
                  onClick={() => {
                    setForm(EMPTY_FORM)
                    setFormSubmitted(false)
                  }}
                >
                  {t('common.modal.cancel', 'Cancel')}
                </Button>
              )}
              <Button
                type="submit"
                loading={formSaving}
                disabled={atCustomLimit || !data}
                leftSection={<Icon path={form.editing ? mdiPencilOutline : mdiPlus} size={0.75} aria-hidden="true" />}
              >
                {form.editing
                  ? t('admin.content.settings.ai_links.save', 'Save provider')
                  : t('admin.content.settings.ai_links.add', 'Add provider')}
              </Button>
            </Group>
          </Stack>
        </form>
      </Paper>

      <Paper withBorder p="md" radius="md">
        <Stack gap="xs">
          <TextInput
            label={t('admin.content.settings.ai_links.test_label', 'Test a link')}
            description={t(
              'admin.content.settings.ai_links.test_description',
              'Checked in this browser against the enabled providers, in server match order.'
            )}
            placeholder="https://chatgpt.com/share/…"
            value={testUrl}
            inputMode="url"
            autoComplete="off"
            spellCheck={false}
            onChange={(event) => setTestUrl(event.currentTarget.value)}
          />
          <div role="status" aria-live="polite">
            {testResult && (
              <Text
                size="sm"
                c={testResult.ok ? (dark ? 'teal' : 'teal.9') : dark ? 'orange' : 'red.9'}
                style={{ overflowWrap: 'anywhere' }}
              >
                {testResult.text}
              </Text>
            )}
          </div>
        </Stack>
      </Paper>
    </Stack>
  )
}
