import { Button, Switch } from '@mantine/core'
import { useCallback, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { assertJsonResponse } from '@Utils/ChallengePolling'
import { showErrorMsg } from '@Utils/Shared'
import { useChallengePolling } from '@Hooks/useChallengePolling'
import api from '@Api'

export function AdPatchActivitySetting({ gameId, disabled }: { gameId: number; disabled: boolean }) {
  const { t } = useTranslation()
  const request = useCallback(
    async (signal: AbortSignal) => assertJsonResponse(await api.edit.getAdPatchActivityPolicy(gameId, { signal })),
    [gameId]
  )
  const { data, error, mutate } = useChallengePolling({
    key: `/api/edit/games/${gameId}/ad/patch-activity`,
    active: true,
    request,
    refreshInterval: 0,
  })
  const pending = useRef(false)
  const [saving, setSaving] = useState(false)
  async function change(enabled: boolean) {
    if (pending.current || disabled || !data?.editable) return
    pending.current = true
    setSaving(true)
    try {
      const response = await api.edit.setAdPatchActivityPolicy(gameId, enabled, data.revision)
      await mutate(assertJsonResponse(response), { revalidate: false })
    } catch (cause) {
      showErrorMsg(cause, t)
      void mutate()
    } finally {
      pending.current = false
      setSaving(false)
    }
  }
  if (error)
    return (
      <Button variant="subtle" onClick={() => void mutate()}>
        {t('admin.ad.patch_activity.retry', 'Reload patch activity setting')}
      </Button>
    )
  return (
    <Switch
      checked={data?.enabled ?? false}
      disabled={disabled || saving || !data?.editable}
      label={t('admin.ad.patch_activity.label', 'Share patch activity')}
      description={t(
        'admin.ad.patch_activity.help',
        'Choose before the event. Deployment counts appear after one additional round finishes. After the event and practice close, teams in the same division can see which starter files marked shareAfterClose changed in deployed revisions.'
      )}
      onChange={(event) => void change(event.currentTarget.checked)}
    />
  )
}
