import { Button, FileInput, Group, Stack, Text } from '@mantine/core'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import useSWR from 'swr'
import api from '@Api'
import { CompletionPollSWRConfig, useCompletionPolling } from '@Hooks/useCompletionPolling'
import { showErrorMsg } from '@Utils/Shared'

interface Props {
  gameId: number
  challengeId: number
  started: boolean
  disabled: boolean
}

export function AdPatchStarterInput({ gameId, challengeId, started, disabled }: Props) {
  const { t } = useTranslation()
  const key = `/api/edit/games/${gameId}/challenges/${challengeId}/patch-starter`
  const { data, error, isValidating, mutate } = useSWR(
    key,
    async () => (await api.edit.getAdPatchStarter(gameId, challengeId)).data,
    CompletionPollSWRConfig
  )
  useCompletionPolling({
    key,
    phase: 'starter-image',
    enabled: !!data?.awaitingImageBuild && !started,
    data,
    error,
    isValidating,
    mutate,
    successDelay: () => 15_000,
  })
  const [file, setFile] = useState<File | null>(null)
  const [saving, setSaving] = useState(false)
  const pending = useRef<AbortController | null>(null)
  useEffect(() => () => pending.current?.abort(), [])

  const save = async () => {
    if (!file || disabled || data === undefined || error || pending.current) return
    const controller = new AbortController()
    pending.current = controller
    setSaving(true)
    try {
      const response = await api.edit.putAdPatchStarter(gameId, challengeId, file, data?.treeId ?? null, {
        signal: controller.signal,
      })
      if (!controller.signal.aborted) {
        await mutate(response.data, { revalidate: false })
        setFile(null)
      }
    } catch (cause) {
      if (!controller.signal.aborted) {
        showErrorMsg(cause, t)
        setFile(null)
        void mutate()
      }
    } finally {
      pending.current = null
      if (!controller.signal.aborted) setSaving(false)
    }
  }
  const tooLarge = !!file && file.size > 72 * 1024 * 1024
  const locked = !!data && started
  return (
    <Stack gap="xs" data-ad-patch-starter>
      <FileInput
        label={t('admin.ad.patch_starter.label', 'Starter ZIP for team patches')}
        description={t('admin.ad.patch_starter.help', 'Choose the source teams may see, with a Dockerfile at the root.')}
        accept=".zip,application/zip"
        clearable
        clearButtonProps={{ 'aria-label': t('admin.ad.patch_starter.clear', 'Clear selected starter ZIP') }}
        value={file}
        onChange={setFile}
        error={tooLarge && t('admin.ad.patch_starter.size', 'Choose a ZIP of 72 MiB or smaller.')}
        disabled={disabled || locked || saving || data === undefined || !!error}
      />
      {data && (
        <Text size="sm" role="status">
          {locked
            ? t('admin.ad.patch_starter.pinned', 'Game started · original tree {{tree}}', { tree: data.treeId.slice(0, 12) })
            : t('admin.ad.patch_starter.saved', 'Saved starter · tree {{tree}}', { tree: data.treeId.slice(0, 12) })}
        </Text>
      )}
      {data && (
        <Text size="sm" c="dimmed" style={{ overflowWrap: 'anywhere' }}>
          {data.runtimeImageRef
            ? t('admin.ad.patch_starter.image', 'Original image · {{image}}', { image: data.runtimeImageRef })
            : data.awaitingImageBuild && !started
              ? t('admin.ad.patch_starter.waiting_image', 'Build the challenge image before the game starts to save it as the original.')
              : t('admin.ad.patch_starter.no_image', 'An original runtime image was never recorded for this ZIP.')}
        </Text>
      )}
      {!!data?.sshSources?.length && <details>
        <summary>{t('admin.ad.patch_starter.ssh_sources', 'Registered SSH source files ({{count}})', { count: data.sshSources.length })}</summary>
        <ul>{data.sshSources.map(({ sourcePath, containerPath, retainContent, shareAfterClose }) => (
          <li key={sourcePath} style={{ overflowWrap: 'anywhere' }}><code>{sourcePath}</code> → <code>{containerPath}</code>
            {retainContent && <> · {t('game.ad.patch.ssh_retained', 'Included in team recovery ZIPs')}</>}
            {shareAfterClose && <> · {t('game.ad.patch.archive_marked', 'File status shared after close when patch activity is enabled')}</>}
          </li>
        ))}</ul>
      </details>}
      {error ? (
        <Group>
          <Text size="sm" role="alert">{t('admin.ad.patch_starter.load_error', 'Could not load the saved starter.')}</Text>
          <Button variant="subtle" size="xs" onClick={() => void mutate()}>{t('common.retry', 'Retry')}</Button>
        </Group>
      ) : file && !locked && (
        <Group>
          <Button size="xs" loading={saving} disabled={disabled || tooLarge} onClick={() => void save()}>
            {data ? t('admin.ad.patch_starter.replace', 'Replace starter ZIP') : t('admin.ad.patch_starter.save', 'Save starter ZIP')}
          </Button>
        </Group>
      )}
    </Stack>
  )
}
