import { Button, FileInput, Group, NativeSelect, Stack, Text } from '@mantine/core'
import { useCallback, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import api, { type AdPatchPreview } from '@Api'
import { useChallengePolling } from '@Hooks/useChallengePolling'
import { downloadBlob } from '@Utils/ApiHelper'
import { assertJsonResponse } from '@Utils/ChallengePolling'
import { httpErrorStatus } from '@Utils/ProfileRetry'
import { showErrorMsg } from '@Utils/Shared'
import { AdPatchHistory } from './AdPatchHistory'
import { AdPatchChangeList } from './AdPatchChangeList'

export function AdPatchPreviewPanel({ gameId, challengeId, serviceId }: { gameId: number; challengeId: number; serviceId: number }) {
  const { t } = useTranslation()
  const request = useCallback(async (signal: AbortSignal) =>
    assertJsonResponse(await api.game.getAdPatchPreviewInfo(gameId, challengeId, { signal })), [gameId, challengeId])
  const { data, error, mutate } = useChallengePolling({
    key: `/api/game/${gameId}/ad/challenges/${challengeId}/patch-preview`,
    active: true, refreshInterval: 0, request,
  })
  const [file, setFile] = useState<File | null>(null)
  const [base, setBase] = useState<'latest' | 'starter'>('latest')
  const [report, setReport] = useState<AdPatchPreview | null>(null)
  const [busy, setBusy] = useState(false)
  const [saving, setSaving] = useState(false)
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
      const response = await api.game.previewAdPatch(gameId, challengeId, file, { base, signal: controller.signal })
      if (!controller.signal.aborted) {
        setReport(assertJsonResponse(response))
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
        accept=".zip,application/zip" clearable disabled={busy || saving} value={file}
        clearButtonProps={{ 'aria-label': t('game.ad.patch.clear', 'Clear selected patch ZIP') }}
        error={tooLarge && t('game.ad.patch.size', 'Choose a ZIP of 72 MiB or smaller.')}
        onChange={next => { setFile(next); setReport(null) }}
      />
      {file && <NativeSelect size="xs" label={t('game.ad.patch.compare_with', 'Compare with')} value={base}
        disabled={busy || saving} data={[
          { value: 'latest', label: t('game.ad.patch.latest_team_source', 'Latest team revision') },
          { value: 'starter', label: t('game.ad.patch.original', 'Starter ZIP') },
        ]} onChange={event => {
          setBase(event.currentTarget.value === 'starter' ? 'starter' : 'latest')
          setReport(null)
        }} />}
      <Group gap="xs">
        <Button size="xs" variant="light" loading={downloading} onClick={() => void downloadBlob(
          `patch-starter:${gameId}:${challengeId}`, () => api.game.downloadAdPatchStarter(gameId, challengeId), setDownloading, t, 'starter.zip'
        )}>{t('game.ad.patch.download', 'Download starter ZIP')}</Button>
        {file && <Button size="xs" loading={busy} disabled={tooLarge || saving} onClick={() => void preview()}>{t('game.ad.patch.compare', 'Compare files')}</Button>}
      </Group>
      {report && (
        <>
          <Text size="sm" role="status">{report.comparisonRevision
            ? t('game.ad.patch.compared_revision', 'Compared with revision {{number}}.', { number: report.comparisonRevision })
            : t('game.ad.patch.compared_starter', 'Compared with the starter ZIP.')} {' '}
            {report.changes.length
              ? t('game.ad.patch.count', 'Files changed: {{count}}', { count: report.changes.length })
              : t('game.ad.patch.no_changes', 'No files changed.')}</Text>
          <AdPatchChangeList changes={report.changes} />
        </>
      )}
      <AdPatchHistory gameId={gameId} challengeId={challengeId} serviceId={serviceId} file={file}
        report={report} saving={saving} setSaving={setSaving} />
    </Stack>
  )
}
