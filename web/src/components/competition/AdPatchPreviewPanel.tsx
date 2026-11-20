import { Button, FileInput, Group, Stack, Text } from '@mantine/core'
import { useCallback, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import api, { type AdPatchPreview } from '@Api'
import { useChallengePolling } from '@Hooks/useChallengePolling'
import { downloadBlob } from '@Utils/ApiHelper'
import { assertJsonResponse } from '@Utils/ChallengePolling'
import { httpErrorStatus } from '@Utils/ProfileRetry'
import { showErrorMsg } from '@Utils/Shared'

export function AdPatchPreviewPanel({ gameId, challengeId }: { gameId: number; challengeId: number }) {
  const { t } = useTranslation()
  const request = useCallback(async (signal: AbortSignal) =>
    assertJsonResponse(await api.game.getAdPatchPreviewInfo(gameId, challengeId, { signal })), [gameId, challengeId])
  const { data, error, mutate } = useChallengePolling({
    key: `/api/game/${gameId}/ad/challenges/${challengeId}/patch-preview`,
    active: true, refreshInterval: 0, request,
  })
  const [file, setFile] = useState<File | null>(null)
  const [report, setReport] = useState<AdPatchPreview | null>(null)
  const [limit, setLimit] = useState(50)
  const [busy, setBusy] = useState(false)
  const [downloading, setDownloading] = useState(false)
  const pending = useRef<AbortController | null>(null)
  useEffect(() => () => pending.current?.abort(), [])

  if (error) {
    const status = httpErrorStatus(error)
    if (status === 401 || status === 403 || status === 404) return null
    return <Button size="xs" variant="subtle" onClick={() => void mutate()}>{t('game.ad.patch.retry', 'Retry patch preview')}</Button>
  }
  if (!data) return null
  const tooLarge = !!file && file.size > 72 * 1024 * 1024
  const preview = async () => {
    if (!file || tooLarge || pending.current) return
    const controller = new AbortController()
    pending.current = controller
    setReport(null)
    setBusy(true)
    try {
      const response = await api.game.previewAdPatch(gameId, challengeId, file, { signal: controller.signal })
      if (!controller.signal.aborted) {
        setReport(assertJsonResponse(response))
        setLimit(50)
      }
    } catch (cause) {
      if (!controller.signal.aborted) showErrorMsg(cause, t)
    } finally {
      pending.current = null
      if (!controller.signal.aborted) setBusy(false)
    }
  }
  return (
    <Stack gap="xs" data-ad-patch-preview>
      <FileInput
        label={t('game.ad.patch.label', 'Preview a patch ZIP')}
        description={t('game.ad.patch.help', 'Compare files with the original starter ZIP. Your service keeps running.')}
        accept=".zip,application/zip" clearable disabled={busy} value={file}
        clearButtonProps={{ 'aria-label': t('game.ad.patch.clear', 'Clear selected patch ZIP') }}
        error={tooLarge && t('game.ad.patch.size', 'Choose a ZIP of 72 MiB or smaller.')}
        onChange={next => { setFile(next); setReport(null) }}
      />
      <Group gap="xs">
        <Button size="xs" variant="light" loading={downloading} onClick={() => void downloadBlob(
          `patch-starter:${gameId}:${challengeId}`, () => api.game.downloadAdPatchStarter(gameId, challengeId), setDownloading, t, 'starter.zip'
        )}>{t('game.ad.patch.download', 'Download starter ZIP')}</Button>
        {file && <Button size="xs" loading={busy} disabled={tooLarge} onClick={() => void preview()}>{t('game.ad.patch.compare', 'Compare files')}</Button>}
      </Group>
      {report && (
        <>
          <Text size="sm" role="status">{report.changes.length
            ? t('game.ad.patch.count', 'Files changed: {{count}}', { count: report.changes.length })
            : t('game.ad.patch.identical', 'Matches the starter ZIP.')}</Text>
          {report.changes.length > 0 && (
            <ul style={{ overflowWrap: 'anywhere', margin: 0, paddingInlineStart: '1.25rem' }}>
              {report.changes.slice(0, limit).map(change => (
                <li key={change.path}><Text size="sm" span>{t(`game.ad.patch.kind.${change.kind}`, change.kind)} · <code>{change.path}</code></Text></li>
              ))}
            </ul>
          )}
          {report.changes.length > limit && <Button variant="subtle" size="xs" onClick={() => setLimit(limit + 50)}>{t('game.ad.patch.more', 'Show more files')}</Button>}
        </>
      )}
    </Stack>
  )
}
