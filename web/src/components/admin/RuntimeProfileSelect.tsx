import { Alert, Button, Select, Stack, Text } from '@mantine/core'
import { useModals } from '@mantine/modals'
import { useTranslation } from 'react-i18next'
import type { RuntimeProfile } from '@Api'

interface Props {
  profiles: RuntimeProfile[]
  value: string | null
  disabled: boolean
  replacementAllowed: boolean
  pending: boolean
  locked: boolean
  onChange: (value: string | null) => void
  onReplace: () => Promise<unknown>
}

export function RuntimeProfileSelect({
  profiles,
  value,
  disabled,
  replacementAllowed,
  pending,
  locked,
  onChange,
  onReplace,
}: Props) {
  const { t } = useTranslation()
  const modals = useModals()
  const available = value === null || profiles.some((profile) => profile.id === value && profile.enabled)
  const choices = [
    { value: '', label: t('admin.runtime_profile.default', 'Deployment default') },
    ...profiles.map((profile) => ({
      value: profile.id,
      disabled: !profile.enabled,
      label: `${profile.name} · ${profile.isolation === 'microvm' ? 'MicroVM (Kata)' : profile.backend === 'docker' ? 'Docker' : 'Kubernetes'}`,
    })),
    ...(!available && value && !profiles.some((profile) => profile.id === value)
      ? [{ value, label: `${value} (${t('admin.runtime_profile.unavailable', 'unavailable')})`, disabled: true }]
      : []),
  ]
  return (
    <Stack gap="xs" data-runtime-profile-controls>
      {pending && (
        <Alert color="orange">
          {t('admin.runtime_profile.pending', 'An interrupted runtime update is pending. Restore the same edits and use Save and replace instances to retry it. The challenge stays disabled until recovery finishes.')}
        </Alert>
      )}
      <Select
        label={t('admin.runtime_profile.label', 'Runtime profile')}
        description={t(
          'admin.runtime_profile.description',
          'Choose where and how new challenge instances run. Saving keeps existing instances on their original runtime.'
        )}
        value={value ?? ''}
        data={choices}
        disabled={disabled || locked}
        allowDeselect={false}
        onChange={(next) => onChange(next || null)}
      />
      {!available && (
        <Alert color="orange">
          {t(
            'admin.runtime_profile.missing',
            'This profile is no longer configured. Ask an administrator to restore it so existing instances can be managed.'
          )}
        </Alert>
      )}
      {locked && (
        <Text size="sm" c="dimmed">
          {t(
            'admin.runtime_profile.locked',
            'A&D/KotH runtime changes are locked after official scoring starts. Multi-service worker definitions and BYOC services use their own runtime configuration.'
          )}
        </Text>
      )}
      <Button
        variant="default"
        size="sm"
        style={{ alignSelf: 'flex-start', maxWidth: '100%', height: 'auto', minHeight: 36 }}
        styles={{ label: { whiteSpace: 'normal', paddingBlock: 8 } }}
        disabled={disabled || locked || !replacementAllowed || !available}
        onClick={() =>
          modals.openConfirmModal({
            title: t('admin.runtime_profile.replace_title', 'Replace running challenge instances?'),
            closeButtonProps: { 'aria-label': t('common.modal.cancel', 'Cancel') },
            children: (
              <Text size="sm">
                {t(
                  'admin.runtime_profile.replace_warning',
                  'This saves your edits and stops all current instances of this challenge, including test instances. Writable files and player modifications will be lost. Shared and defending services are reprovisioned; Jeopardy players start fresh instances. Scores and submissions are preserved.'
                )}
              </Text>
            ),
            labels: {
              confirm: t('admin.runtime_profile.replace_confirm', 'Save and replace instances'),
              cancel: t('common.modal.cancel', 'Cancel'),
            },
            confirmProps: { color: 'red' },
            onConfirm: () => {
              void onReplace()
            },
          })
        }
      >
        {t('admin.runtime_profile.replace_confirm', 'Save and replace instances')}
      </Button>
    </Stack>
  )
}
