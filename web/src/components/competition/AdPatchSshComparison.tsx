import { Button, Stack, Text } from '@mantine/core'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import api, { type AdSshComparison } from '@Api'
import { assertJsonResponse } from '@Utils/ChallengePolling'
import { showErrorMsg } from '@Utils/Shared'
import { AdPatchChangeList } from './AdPatchChangeList'

interface Props {
  gameId: number
  challengeId: number
  serviceId: number
  revision: number
  commitId: string
}

export function AdPatchSshComparison({ gameId, challengeId, serviceId, revision, commitId }: Props) {
  const { t } = useTranslation()
  const [result, setResult] = useState<AdSshComparison>()
  const [busy, setBusy] = useState(false)
  const pending = useRef<AbortController | null>(null)
  useEffect(() => () => pending.current?.abort(), [])
  const compare = async () => {
    if (pending.current) return
    const controller = new AbortController()
    pending.current = controller
    setBusy(true)
    setResult(undefined)
    try {
      const response = await api.game.compareAdPatchWithSsh(gameId, challengeId, revision, serviceId, commitId, { signal: controller.signal })
      if (!controller.signal.aborted) setResult(assertJsonResponse(response))
    } catch (cause) {
      if (!controller.signal.aborted) showErrorMsg(cause, t)
    } finally {
      pending.current = null
      if (!controller.signal.aborted) setBusy(false)
    }
  }
  const report = result?.report
  return <Stack gap={2} align="flex-start">
    <Button size="xs" variant="light" loading={busy} onClick={() => void compare()}>
      {t('game.ad.patch.compare_ssh', 'Compare ZIP with last SSH report')}
    </Button>
    {result && <Text size="sm" role="status">{report?.trackedFiles
      ? t('game.ad.patch.ssh_result', 'SSH report {{number}} ({{time}}). Registered files compared: {{files}}. Differences in this ZIP: {{changes}}.', {
        number: report.revision, time: new Date(report.lastSeen).toLocaleString(), files: report.trackedFiles, changes: report.changes.length,
      })
      : t('game.ad.patch.ssh_unavailable', 'No readable SSH report with registered files is available for this service.')}</Text>}
    {report && <AdPatchChangeList changes={report.changes} />}
  </Stack>
}
