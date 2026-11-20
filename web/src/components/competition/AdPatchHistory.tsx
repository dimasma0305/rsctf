import { Button, Group, Stack, Text } from '@mantine/core'
import { useCallback, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import api, { type AdPatchPreview } from '@Api'
import { useChallengePolling } from '@Hooks/useChallengePolling'
import { assertJsonResponse } from '@Utils/ChallengePolling'
import { showErrorMsg } from '@Utils/Shared'
import { downloadBlob } from '@Utils/ApiHelper'
import { AdPatchChangeList } from './AdPatchChangeList'

interface Props {
  gameId: number
  challengeId: number
  serviceId: number
  file: File | null
  report: AdPatchPreview | null
  saving: boolean
  setSaving: (saving: boolean) => void
}

export function AdPatchHistory({ gameId, challengeId, serviceId, file, report, saving, setSaving }: Props) {
  const { t } = useTranslation()
  const [before, setBefore] = useState<number>()
  const [selected, setSelected] = useState<number>()
  const [notice, setNotice] = useState('')
  const pending = useRef<AbortController | null>(null)
  useEffect(() => () => pending.current?.abort(), [])
  const request = useCallback(async (signal: AbortSignal) =>
    assertJsonResponse(await api.game.listAdPatchRevisions(gameId, challengeId, before, { signal })), [gameId, challengeId, before])
  const { data, error, mutate } = useChallengePolling({
    key: `/api/game/${gameId}/ad/challenges/${challengeId}/patch-revisions:${serviceId}:${before ?? 'latest'}`,
    active: true, refreshInterval: 0, request,
  })
  const head = data?.[0]
  const outdated = !!report && (head?.revision ?? 0) > report.baseRevision
  const changed = report && (report.comparisonRevision !== undefined && !outdated
    ? report.changes.length > 0
    : report.candidateTree !== (head?.treeId ?? report.starterTree))
  const save = async () => {
    if (!file || !report || !data || before || error || outdated || pending.current) return
    const controller = new AbortController()
    pending.current = controller
    setSaving(true)
    setNotice('')
    try {
      const saved = assertJsonResponse(await api.game.saveAdPatchRevision(gameId, challengeId, serviceId, file, report.baseRevision, { signal: controller.signal }))
      if (controller.signal.aborted) return
      setNotice(t('game.ad.patch.saved_revision', 'Revision {{number}} saved. Your service keeps running.', { number: saved.revision }))
      await mutate()
    } catch (cause) {
      if (!controller.signal.aborted) {
        showErrorMsg(cause, t)
        void mutate().catch(() => {})
      }
    } finally {
      pending.current = null
      if (!controller.signal.aborted) setSaving(false)
    }
  }
  if (error) return <Button size="xs" variant="subtle" onClick={() => void mutate().catch(() => {})}>{t('game.ad.patch.history_retry', 'Reload revision history')}</Button>
  if (!data) return null
  return (
    <Stack gap="xs">
      {file && changed && !before && (outdated
        ? <Text size="sm" role="status">{t('game.ad.patch.recompare', 'A teammate saved a revision. Compare files again before saving.')}</Text>
        : <Group><Button size="xs" loading={saving} onClick={() => void save()}>{t('game.ad.patch.save_revision', 'Save private revision')}</Button></Group>)}
      {notice && <Text size="sm" role="status">{notice}</Text>}
      {(data.length > 0 || before) && <details>
        <summary>{t('game.ad.patch.history', 'Team revision history')}</summary>
        {data.map(r => <Stack key={r.revision} gap={2}>
          <Group gap="xs">
            <Button size="compact-sm" variant="subtle" aria-expanded={selected === r.revision}
              onClick={() => setSelected(selected === r.revision ? undefined : r.revision)}>
              {t('game.ad.patch.revision_changes', 'Changes in revision {{number}}', { number: r.revision })}
            </Button>
            <Text size="sm"><code>{r.commitId.slice(0, 12)}</code> · <time dateTime={new Date(r.createdAt).toISOString()}>{new Date(r.createdAt).toLocaleString()}</time></Text>
          </Group>
          {selected === r.revision && <RevisionChanges gameId={gameId} challengeId={challengeId} serviceId={serviceId} revision={r.revision} commitId={r.commitId} />}
        </Stack>)}
        <Group gap="xs">
          {before && <Button size="xs" variant="subtle" onClick={() => setBefore(undefined)}>{t('game.ad.patch.latest', 'Latest revisions')}</Button>}
          {data.length > 0 && data[data.length - 1].revision > 1 && <Button size="xs" variant="subtle" disabled={saving} onClick={() => setBefore(data[data.length - 1].revision)}>{t('game.ad.patch.older', 'Older revisions')}</Button>}
        </Group>
      </details>}
    </Stack>
  )
}

function RevisionChanges({ gameId, challengeId, serviceId, revision, commitId }: Pick<Props, 'gameId' | 'challengeId' | 'serviceId'> & { revision: number; commitId: string }) {
  const { t } = useTranslation()
  const [downloading, setDownloading] = useState(false)
  const request = useCallback(async (signal: AbortSignal) =>
    assertJsonResponse(await api.game.getAdPatchRevisionChanges(gameId, challengeId, revision, { signal })), [gameId, challengeId, revision])
  const { data, error, mutate } = useChallengePolling({
    key: `/api/game/${gameId}/ad/challenges/${challengeId}/patch-revisions/${revision}/changes:${serviceId}`,
    active: true, refreshInterval: 0, request,
  })
  if (error) return <Button size="xs" variant="subtle" onClick={() => void mutate().catch(() => {})}>{t('game.ad.patch.changes_retry', 'Reload file changes')}</Button>
  if (!data) return <Text size="sm" role="status">{t('game.ad.patch.loading_changes', 'Loading file changes…')}</Text>
  return <>
    <AdPatchChangeList changes={data} />
    <Group><Button size="xs" variant="light" loading={downloading} onClick={() => void downloadBlob(
      `patch-revision:${serviceId}:${commitId}`, () => api.game.downloadAdPatchRevision(gameId, challengeId, revision, commitId),
      setDownloading, t, `patch-revision-${revision}.zip`
    )}>{t('game.ad.patch.download_revision', 'Download revision {{number}} ZIP', { number: revision })}</Button></Group>
  </>
}
