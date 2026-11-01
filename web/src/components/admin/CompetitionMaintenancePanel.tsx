import { Alert, Button, Group, Stack, Text } from '@mantine/core'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { createOperationId, startControlJob, waitForControlJob } from '@Utils/ControlJobs'
import { showErrorMsg } from '@Utils/Shared'
import { useCompetitionMaintenance } from '@Hooks/useCompetitionMaintenance'
import api, { ContentType, type ControlJobModel } from '@Api'

export const CompetitionMaintenancePanel = ({
  gameId,
  unsavedChanges = false,
}: {
  gameId: number
  unsavedChanges?: boolean
}) => {
  const { t } = useTranslation()
  const { data: window, mutate, error } = useCompetitionMaintenance(gameId)
  const [busy, setBusy] = useState(false)
  const requestId = useRef<string | null>(null)
  const resumeId = useRef<string | null>(null)
  const abort = useRef(new AbortController())
  useEffect(() => {
    const controller = new AbortController()
    abort.current = controller
    requestId.current = null
    resumeId.current = null
    return () => controller.abort()
  }, [gameId])
  const editing = window?.state === 'Editing'
  const applying = window?.state === 'Applying'
  const requested = window?.state === 'Requested'
  const start = async () => {
    setBusy(true)
    try {
      requestId.current ??= createOperationId()
      await api.request({
        path: `/api/edit/games/${gameId}/ad/maintenance`,
        method: 'POST',
        type: ContentType.Json,
        body: { operationId: requestId.current },
      })
      requestId.current = null
      await mutate()
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      setBusy(false)
    }
  }
  const resume = async () => {
    if (unsavedChanges) return
    setBusy(true)
    try {
      resumeId.current ??= createOperationId()
      const operationId = resumeId.current
      const job = await startControlJob(
        operationId,
        () =>
          api.request<ControlJobModel>({
            path: `/api/edit/games/${gameId}/ad/maintenance/resume`,
            method: 'POST',
            type: ContentType.Json,
            body: { operationId },
          }),
        abort.current.signal
      )
      await waitForControlJob(job, abort.current.signal)
      resumeId.current = null
    } catch (error) {
      resumeId.current = null
      if (!abort.current.signal.aborted) showErrorMsg(error, t)
    } finally {
      await mutate()
      setBusy(false)
    }
  }
  const cancel = async () => {
    if (!window) return
    setBusy(true)
    try {
      await api.request({
        path: `/api/edit/games/${gameId}/ad/maintenance/cancel`,
        method: 'POST',
        type: ContentType.Json,
        body: { operationId: window.id },
      })
      await mutate()
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Alert
      data-competition-maintenance
      color={editing || applying ? 'orange' : 'blue'}
      styles={{ title: { color: 'var(--mantine-color-text)' } }}
      title={t('admin.maintenance.title', 'Live challenge updates')}
    >
      <Stack gap="xs">
        <Text size="sm" role="status">
          {requested
            ? t(
                'admin.maintenance.waiting',
                'Scoring will pause after the current A&D round or KoTH epoch finishes. You can keep editing titles and descriptions.'
              )
            : editing
              ? t(
                  'admin.maintenance.editing',
                  'Scoring is paused. Edit images, checkers, resources, ports, runtime settings or observers, then build and apply your changes.'
                )
              : applying
                ? t(
                    'admin.maintenance.applying',
                    'Applying changes. Scoring stays paused until replacement is safe. You can retry if the operation fails.'
                  )
                : t(
                    'admin.maintenance.ready',
                    'Need to fix a live challenge? Open a maintenance window. Past scores and evidence stay unchanged.'
                  )}
        </Text>
        {window?.effectiveFromRound != null && (
          <Text size="xs">
            {t('admin.maintenance.round', 'Changes take effect from round {{round}}.', {
              round: window.effectiveFromRound,
            })}
          </Text>
        )}
        {(window?.error || error) && (
          <Text size="sm" c="red" role="alert">
            {window?.error || t('admin.maintenance.load_error', 'Could not read maintenance status. Refresh to retry.')}
          </Text>
        )}
        {editing && unsavedChanges && (
          <Text size="sm">
            {t('admin.maintenance.save_first', 'Save your challenge changes before applying and resuming scoring.')}
          </Text>
        )}
        <Group wrap="wrap">
          {requested && (
            <Button size="xs" variant="subtle" disabled={busy} onClick={cancel}>
              {t('common.button.cancel', 'Cancel request')}
            </Button>
          )}
          {editing || applying ? (
            <Button size="xs" loading={busy} disabled={unsavedChanges} onClick={resume}>
              {t('admin.maintenance.apply', 'Apply changes and resume')}
            </Button>
          ) : (
            <Button size="xs" loading={busy} disabled={requested || !!error} onClick={start}>
              {t('admin.maintenance.open', 'Open maintenance window')}
            </Button>
          )}
        </Group>
      </Stack>
    </Alert>
  )
}
