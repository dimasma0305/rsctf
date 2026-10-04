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
} from '@mantine/core'
import { modals } from '@mantine/modals'
import { mdiAlertCircleOutline, mdiDeleteOutline, mdiPencilOutline, mdiPlus } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, FormEvent, useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import {
  AGENT_SIGNATURE_LABEL_MAX_LENGTH,
  AGENT_SIGNATURE_PATTERN_MAX_LENGTH,
  agentSignatureKeyProblem,
} from '@Utils/AgentSignatures'
import { showErrorMsg, showSuccessMsg, tryGetErrorMsg } from '@Utils/Shared'
import api, { AgentSignatureListModel, AgentSignatureModel } from '@Api'

/** Shared with the settings status map so both read one cache entry. */
export const AGENT_SIGNATURES_READ_CONFIG = {
  refreshInterval: 0,
  revalidateOnFocus: false,
  revalidateOnReconnect: false,
}

export const useAgentSignatures = (enabled = true) =>
  api.admin.useAdminGetAgentSignatures(AGENT_SIGNATURES_READ_CONFIG, enabled)

interface SignatureForm {
  /** Existing custom key when editing; null when creating. */
  editing: string | null
  key: string
  label: string
  pattern: string
}

const EMPTY_FORM: SignatureForm = { editing: null, key: '', label: '', pattern: '' }

export const AgentSignaturesSettings: FC = () => {
  const { t } = useTranslation()
  const { data, error, mutate } = useAgentSignatures()
  const [pending, setPending] = useState<string | null>(null)
  const [form, setForm] = useState<SignatureForm>(EMPTY_FORM)
  const [formSubmitted, setFormSubmitted] = useState(false)
  const [formSaving, setFormSaving] = useState(false)
  const formHeadingId = useId()

  const signatures = data?.signatures ?? []
  const builtins = signatures.filter((signature) => signature.builtin)
  const custom = signatures.filter((signature) => !signature.builtin)
  const maxCustom = data?.maxCustomSignatures ?? 0
  const atCustomLimit = form.editing === null && custom.length >= maxCustom

  const replaceSignature = (list: AgentSignatureListModel, saved: AgentSignatureModel): AgentSignatureListModel => {
    const exists = list.signatures.some((signature) => signature.key === saved.key)
    return {
      ...list,
      signatures: exists
        ? list.signatures.map((signature) => (signature.key === saved.key ? saved : signature))
        : [...list.signatures, saved],
    }
  }

  const onToggle = async (signature: AgentSignatureModel, enabled: boolean) => {
    if (pending) return
    setPending(signature.key)
    try {
      // Built-in keys accept only `enabled`; custom signatures keep their rule.
      const body = signature.builtin ? { enabled } : { enabled, label: signature.label, pattern: signature.pattern }
      const { data: saved } = await api.admin.adminSaveAgentSignature(signature.key, body)
      await mutate((current) => (current ? replaceSignature(current, saved) : current), { revalidate: false })
    } catch (toggleError) {
      showErrorMsg(toggleError, t)
    } finally {
      setPending(null)
    }
  }

  const onDelete = (signature: AgentSignatureModel) =>
    modals.openConfirmModal({
      title: t('admin.content.settings.agent_signatures.delete_title', 'Delete {{signature}}?', {
        signature: signature.label,
      }),
      children: (
        <Text size="sm">
          {t(
            'admin.content.settings.agent_signatures.delete_message',
            'New uploads are no longer checked for this trace. Evidence already recorded stays in the cheat report.'
          )}
        </Text>
      ),
      labels: {
        confirm: t('admin.content.settings.agent_signatures.delete', 'Delete'),
        cancel: t('common.modal.cancel', 'Cancel'),
      },
      confirmProps: { color: 'red' },
      onConfirm: async () => {
        setPending(signature.key)
        try {
          await api.admin.adminDeleteAgentSignature(signature.key)
          await mutate(
            (current) =>
              current
                ? { ...current, signatures: current.signatures.filter((item) => item.key !== signature.key) }
                : current,
            { revalidate: false }
          )
          if (form.editing === signature.key) setForm(EMPTY_FORM)
          showSuccessMsg(t('admin.content.settings.agent_signatures.deleted', 'Signature deleted'))
        } catch (deleteError) {
          showErrorMsg(deleteError, t)
        } finally {
          setPending(null)
        }
      },
    })

  const keyProblem = form.editing === null ? agentSignatureKeyProblem(form.key, signatures) : null
  const keyError =
    keyProblem === 'format'
      ? t(
          'admin.content.settings.agent_signatures.key_format',
          'Use 1–40 lowercase letters, digits, or hyphens, starting with a letter or digit.'
        )
      : keyProblem === 'builtin'
        ? t('admin.content.settings.agent_signatures.key_builtin', 'This key belongs to a built-in signature.')
        : keyProblem === 'duplicate'
          ? t('admin.content.settings.agent_signatures.key_duplicate', 'A custom signature already uses this key.')
          : null
  const label = form.label.trim()
  const labelError =
    label.length === 0 || label.length > AGENT_SIGNATURE_LABEL_MAX_LENGTH
      ? t('admin.content.settings.agent_signatures.label_invalid', 'Enter a label of 1–64 characters.')
      : null
  const pattern = form.pattern.trim()
  const patternError =
    pattern.length === 0 || pattern.length > AGENT_SIGNATURE_PATTERN_MAX_LENGTH
      ? t('admin.content.settings.agent_signatures.pattern_length', 'Enter a pattern of 1–512 characters.')
      : null
  const formValid = !keyError && !labelError && !patternError && !atCustomLimit

  const onSubmitForm = async (event: FormEvent) => {
    event.preventDefault()
    setFormSubmitted(true)
    if (!formValid || formSaving) return
    const key = form.editing ?? form.key
    const existing = signatures.find((signature) => signature.key === key)
    setFormSaving(true)
    try {
      const { data: saved } = await api.admin.adminSaveAgentSignature(key, {
        enabled: existing?.enabled ?? true,
        label,
        pattern,
      })
      await mutate((current) => (current ? replaceSignature(current, saved) : current), { revalidate: false })
      showSuccessMsg(
        form.editing
          ? t('admin.content.settings.agent_signatures.updated', 'Signature updated')
          : t('admin.content.settings.agent_signatures.created', 'Signature added')
      )
      setForm(EMPTY_FORM)
      setFormSubmitted(false)
    } catch (saveError) {
      showErrorMsg(saveError, t)
    } finally {
      setFormSaving(false)
    }
  }

  const signatureCard = (signature: AgentSignatureModel) => (
    <Paper key={signature.key} withBorder p="sm" radius="md" component="li" data-agent-signature={signature.key}>
      <Stack gap={6}>
        <Group justify="space-between" gap="xs" wrap="wrap">
          <Group gap={6} wrap="wrap" style={{ minWidth: 0 }}>
            <Text fw={600}>{signature.label}</Text>
            <Badge size="sm" variant="light" color="gray" tt="none" ff="monospace">
              {signature.key}
            </Badge>
          </Group>
          <Switch
            label={t('admin.content.settings.agent_signatures.enabled', 'Enabled')}
            aria-label={t('admin.content.settings.agent_signatures.enabled_for', 'Enabled: {{signature}}', {
              signature: signature.label,
            })}
            checked={signature.enabled}
            disabled={pending !== null}
            onChange={(event) => void onToggle(signature, event.currentTarget.checked)}
          />
        </Group>
        <Code block style={{ whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }}>
          {signature.pattern}
        </Code>
        {signature.examples.length > 0 && (
          <Stack gap={2}>
            <Text size="xs" c="dimmed">
              {t('admin.content.settings.agent_signatures.examples', 'Examples')}
            </Text>
            {signature.examples.map((example) => (
              <Text key={example} size="xs" ff="monospace" style={{ overflowWrap: 'anywhere' }}>
                {example}
              </Text>
            ))}
          </Stack>
        )}
        {!signature.builtin && (
          <Group gap="xs" justify="flex-end">
            <Button
              size="compact-sm"
              variant="default"
              disabled={pending !== null}
              leftSection={<Icon path={mdiPencilOutline} size={0.7} aria-hidden="true" />}
              aria-label={t('admin.content.settings.agent_signatures.edit_named', 'Edit {{signature}}', {
                signature: signature.label,
              })}
              onClick={() => {
                setForm({
                  editing: signature.key,
                  key: signature.key,
                  label: signature.label,
                  pattern: signature.pattern,
                })
                setFormSubmitted(false)
              }}
            >
              {t('admin.content.settings.agent_signatures.edit', 'Edit')}
            </Button>
            <Button
              size="compact-sm"
              variant="light"
              color="red"
              disabled={pending !== null}
              loading={pending === signature.key}
              leftSection={<Icon path={mdiDeleteOutline} size={0.7} aria-hidden="true" />}
              aria-label={t('admin.content.settings.agent_signatures.delete_named', 'Delete {{signature}}', {
                signature: signature.label,
              })}
              onClick={() => onDelete(signature)}
            >
              {t('admin.content.settings.agent_signatures.delete', 'Delete')}
            </Button>
          </Group>
        )}
      </Stack>
    </Paper>
  )

  const showFormErrors = formSubmitted || undefined
  const list = (items: AgentSignatureModel[]) => (
    <SimpleGrid type="container" cols={{ base: 1, '48em': 2 }} component="ul" m={0} p={0} style={{ listStyle: 'none' }}>
      {items.map(signatureCard)}
    </SimpleGrid>
  )

  return (
    <Stack gap="sm" data-agent-signatures>
      <Title order={2}>{t('admin.content.settings.agent_signatures.title', 'Agent signatures')}</Title>
      <Text size="sm" c="dimmed">
        {t(
          'admin.content.settings.agent_signatures.description',
          'Traces that AI agent tools leave in files, such as a coding agent’s session scratchpad path. Uploaded solvers and writeups are scanned for them (never executed); a match is added to the cheat report for review, and a match on a challenge the team declared “No AI used” is flagged as a contradiction. A match shows which tool touched the file, not that a rule was broken.'
        )}
      </Text>
      <Divider />
      {error && !data && (
        <Alert color="red" icon={<Icon path={mdiAlertCircleOutline} size={0.9} aria-hidden="true" />}>
          <Group justify="space-between" gap="sm">
            <Text size="sm">{tryGetErrorMsg(error, t)}</Text>
            <Button size="compact-sm" variant="default" onClick={() => void mutate()}>
              {t('common.button.retry', 'Retry')}
            </Button>
          </Group>
        </Alert>
      )}

      <Title order={3} size="h4">
        {t('admin.content.settings.agent_signatures.builtin_title', 'Built-in signatures')}
      </Title>
      {list(builtins)}

      <Group justify="space-between" gap="xs" mt="sm">
        <Title order={3} size="h4">
          {t('admin.content.settings.agent_signatures.custom_title', 'Custom signatures')}
        </Title>
        <Text size="xs" c="dimmed">
          {t('admin.content.settings.agent_signatures.custom_count', '{{count}} of {{max}}', {
            count: custom.length,
            max: maxCustom,
          })}
        </Text>
      </Group>
      {custom.length === 0 ? (
        <Text size="sm" c="dimmed">
          {t('admin.content.settings.agent_signatures.custom_empty', 'No custom signatures yet.')}
        </Text>
      ) : (
        list(custom)
      )}

      <Paper withBorder p="md" radius="md" component="section" aria-labelledby={formHeadingId}>
        <form onSubmit={(event) => void onSubmitForm(event)} noValidate>
          <Stack gap="sm">
            <Title order={3} size="h5" id={formHeadingId}>
              {form.editing
                ? t('admin.content.settings.agent_signatures.edit_title', 'Edit {{key}}', { key: form.editing })
                : t('admin.content.settings.agent_signatures.add_title', 'Add signature')}
            </Title>
            {atCustomLimit && (
              <Text size="sm" c="orange">
                {t('admin.content.settings.agent_signatures.custom_limit', 'The custom signature limit is reached.')}
              </Text>
            )}
            <SimpleGrid type="container" cols={{ base: 1, '32em': 2 }}>
              <TextInput
                label={t('admin.content.settings.agent_signatures.key', 'Key')}
                description={t('admin.content.settings.agent_signatures.key_description', 'Lowercase, e.g. my-agent')}
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
                label={t('admin.content.settings.agent_signatures.label', 'Label')}
                description={t(
                  'admin.content.settings.agent_signatures.label_description',
                  'Shown to organizers in the cheat report'
                )}
                value={form.label}
                required
                maxLength={AGENT_SIGNATURE_LABEL_MAX_LENGTH}
                error={showFormErrors && labelError}
                onChange={(event) => setForm({ ...form, label: event.currentTarget.value })}
              />
            </SimpleGrid>
            <Textarea
              label={t('admin.content.settings.agent_signatures.pattern', 'Pattern')}
              description={t(
                'admin.content.settings.agent_signatures.pattern_description',
                'Regular expression searched for anywhere in the file, e.g. /opt/my-agent/runs/[0-9]+. It must not match ordinary solver or writeup text.'
              )}
              value={form.pattern}
              required
              autosize
              minRows={2}
              maxLength={AGENT_SIGNATURE_PATTERN_MAX_LENGTH}
              spellCheck={false}
              styles={{ input: { fontFamily: 'monospace' } }}
              error={showFormErrors && patternError}
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
                  ? t('admin.content.settings.agent_signatures.save', 'Save signature')
                  : t('admin.content.settings.agent_signatures.add', 'Add signature')}
              </Button>
            </Group>
          </Stack>
        </form>
      </Paper>
      <Text size="xs" c="dimmed">
        {t(
          'admin.content.settings.agent_signatures.rescan_hint',
          'Changes apply to new uploads. To check files uploaded earlier, use “Rescan uploads” on an event’s cheat detection page.'
        )}
      </Text>
    </Stack>
  )
}
