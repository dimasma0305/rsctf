import { Alert, Badge, Button, Group, Stack, Text, Title } from '@mantine/core'
import dayjs from 'dayjs'
import { useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import useSWR from 'swr'
import api, { type ImagePreflightCapacity, type ImagePreflightModel } from '@Api'
import { CompletionPollSWRConfig, useCompletionPolling } from '@Hooks/useCompletionPolling'
import { createOperationId, startControlJob } from '@Utils/ControlJobs'
import {
  PREFLIGHT_STATE_COLOR,
  formatPreflightBytes,
  formatPreflightCpu,
  formatPreflightDuration,
  isPreflightActive,
  preflightPollDelay,
  preflightRowState,
} from '@Utils/ImagePreflight'
import { showErrorMsg } from '@Utils/Shared'
import classes from '@Styles/EventReadiness.module.css'

interface ImagePreflightPanelProps {
  eventId: number
  enabled: boolean
}

const CapacityTable = ({ capacity }: { capacity: ImagePreflightCapacity }) => {
  const { t } = useTranslation()
  const available = capacity.available ?? null
  const rows: { key: string; requested: string; available: string | null }[] = [
    {
      key: 'cpu',
      requested: formatPreflightCpu(capacity.requested.cpuMillis),
      available: available ? formatPreflightCpu(available.cpuMillis) : null,
    },
    {
      key: 'memory',
      requested: formatPreflightBytes(capacity.requested.memoryBytes),
      available: available ? formatPreflightBytes(available.memoryBytes) : null,
    },
    {
      key: 'storage',
      requested: formatPreflightBytes(capacity.requested.storageBytes),
      available: null,
    },
    { key: 'replicas', requested: String(capacity.requested.replicas), available: null },
    {
      key: 'instances',
      requested: String(capacity.requested.slots),
      available: available ? String(available.slots) : null,
    },
  ]
  return (
    <Stack gap={4} data-preflight-capacity>
      <Text size="sm" fw={600}>
        {t('admin.readiness.preflight.capacity.title')}
      </Text>
      <Text size="xs" c="dimmed">
        {t('admin.readiness.preflight.capacity.basis', {
          teams: capacity.acceptedTeams,
          instances: capacity.instances,
        })}
      </Text>
      <dl className={classes.preflightCapacity}>
        {rows.map((row) => (
          <div key={row.key}>
            <dt>{t(`admin.readiness.preflight.capacity.${row.key}`)}</dt>
            <dd>
              {t('admin.readiness.preflight.capacity.requested', { value: row.requested })}
              {row.available !== null && (
                <>
                  {' · '}
                  {t('admin.readiness.preflight.capacity.available', { value: row.available })}
                </>
              )}
            </dd>
          </div>
        ))}
      </dl>
      <Text size="xs" c="dimmed">
        {available
          ? t('admin.readiness.preflight.capacity.worker_note', {
              workers: available.workers,
              cpu: formatPreflightCpu(capacity.workerRequested.cpuMillis),
              memory: formatPreflightBytes(capacity.workerRequested.memoryBytes),
            })
          : t('admin.readiness.preflight.capacity.unavailable')}
      </Text>
    </Stack>
  )
}

export const ImagePreflightPanel = ({ eventId, enabled }: ImagePreflightPanelProps) => {
  const { t } = useTranslation()
  const key = enabled ? `/api/edit/games/${eventId}/preflight` : ''
  const query = useSWR<ImagePreflightModel>(
    key || null,
    async () => (await api.eventSecurity.getImagePreflight(eventId)).data,
    CompletionPollSWRConfig
  )
  const [starting, setStarting] = useState(false)
  const preflightFlight = useRef<Promise<void> | null>(null)

  useCompletionPolling({
    key,
    phase: 'image-preflight',
    enabled,
    data: query.data,
    error: query.error,
    isValidating: query.isValidating,
    mutate: query.mutate,
    successDelay: preflightPollDelay,
  })

  const data = query.data
  const job = data?.job ?? null
  const summary = data?.summary ?? null
  const results = data?.results ?? []
  const active = isPreflightActive(job)
  const loading = enabled && data === undefined && query.error === undefined

  const start = () => {
    if (preflightFlight.current) return preflightFlight.current
    const operationId = createOperationId()
    const task = (async () => {
      setStarting(true)
      try {
        const started = await startControlJob(operationId, () =>
          api.eventSecurity.startImagePreflight(eventId, operationId)
        )
        // The active job re-arms the completion poller; results follow.
        await query.mutate({ job: started, summary: null, results: [] }, { revalidate: true })
      } catch (error) {
        showErrorMsg(error, t)
      } finally {
        setStarting(false)
        preflightFlight.current = null
      }
    })()
    preflightFlight.current = task
    return task
  }

  const status = job
    ? active
      ? t('admin.readiness.preflight.progress', {
          state: t(`admin.readiness.preflight.job.${job.status}`),
          current: job.progressCurrent,
          total: job.progressTotal,
        })
      : summary
        ? t('admin.readiness.preflight.summary', {
            succeeded: summary.succeeded,
            failed: summary.failed,
            skipped: summary.skipped,
            images: summary.images,
            challenges: summary.challenges,
          })
        : t(`admin.readiness.preflight.job.${job.status}`)
    : loading
      ? t('admin.readiness.preflight.loading')
      : t('admin.readiness.preflight.none')

  return (
    <section className={classes.manual} aria-labelledby="readiness-preflight" data-image-preflight>
      <Stack gap="sm">
        <Group justify="space-between" align="center" gap="sm">
          <Title order={2} size="h5" id="readiness-preflight">
            {t('admin.readiness.preflight.title')}
          </Title>
          <Button
            variant="default"
            mih={44}
            onClick={start}
            loading={starting}
            disabled={!enabled || active}
            data-preflight-run
          >
            {t(active ? 'admin.readiness.preflight.running' : 'admin.readiness.preflight.run')}
          </Button>
        </Group>
        <Text size="sm" c="dimmed">
          {t('admin.readiness.preflight.intro')}
        </Text>
        <Group gap="xs" wrap="wrap">
          {job && (
            <Badge
              color={active ? 'blue' : job.status === 'Succeeded' ? 'teal' : job.status === 'Cancelled' ? 'gray' : 'red'}
              variant="light"
              autoContrast
            >
              {t(`admin.readiness.preflight.job.${job.status}`)}
            </Badge>
          )}
          <Text size="sm" role="status" aria-live="polite" style={{ overflowWrap: 'anywhere' }}>
            {status}
          </Text>
        </Group>
        {job && (
          <Text size="xs" c="dimmed">
            {t('admin.readiness.preflight.started', { time: dayjs(job.createdAtUtc).format('YYYY-MM-DD HH:mm') })}
            {job.finishedAtUtc
              ? ` · ${t('admin.readiness.preflight.finished', { time: dayjs(job.finishedAtUtc).format('YYYY-MM-DD HH:mm') })}`
              : ''}
          </Text>
        )}
        {query.error && !loading && (
          <Alert color="orange" role="alert" title={t('admin.readiness.preflight.load_failed')}>
            {t('admin.readiness.preflight.load_failed_note')}
          </Alert>
        )}
        {job?.error && (
          <Alert color="red" role="alert" title={t('admin.readiness.preflight.job_failed')}>
            <Text size="sm" style={{ overflowWrap: 'anywhere' }}>
              {job.error}
            </Text>
          </Alert>
        )}
        {summary && <CapacityTable capacity={summary.capacity} />}
        {results.length > 0 && (
          <ul className={classes.preflightList} aria-label={t('admin.readiness.preflight.list_label')}>
            {results.map((row) => {
              const state = preflightRowState(row)
              return (
                <li key={row.challengeId} className={classes.preflightRow} data-preflight-state={state}>
                  <Group justify="space-between" gap="xs" wrap="wrap">
                    <Text fw={600} style={{ overflowWrap: 'anywhere' }}>
                      {row.challengeTitle}
                    </Text>
                    <Group gap={6} wrap="wrap">
                      <Badge color="gray" variant="outline">
                        {row.backend}
                      </Badge>
                      <Badge color={PREFLIGHT_STATE_COLOR[state]} variant="light" autoContrast>
                        {t(`admin.readiness.preflight.states.${state}`)}
                      </Badge>
                    </Group>
                  </Group>
                  <Text component="code" size="xs" className={classes.preflightImage}>
                    {row.image || t('admin.readiness.preflight.no_image')}
                  </Text>
                  <Text size="xs" c="dimmed">
                    {t('admin.readiness.preflight.steps.pull')}: {t(`admin.readiness.preflight.step.${row.pullStatus}`)}
                    {' · '}
                    {t('admin.readiness.preflight.steps.start')}:{' '}
                    {t(`admin.readiness.preflight.step.${row.startStatus}`)}
                    {' · '}
                    {formatPreflightDuration(row.durationMs)}
                  </Text>
                  {row.error && (
                    <Text size="sm" style={{ overflowWrap: 'anywhere' }}>
                      <Text component="span" fw={600}>
                        {t('admin.readiness.preflight.error_label')}
                      </Text>{' '}
                      {row.error}
                    </Text>
                  )}
                </li>
              )
            })}
          </ul>
        )}
        {job && !active && results.length === 0 && (
          <Text size="sm" c="dimmed">
            {t('admin.readiness.preflight.empty')}
          </Text>
        )}
      </Stack>
    </section>
  )
}
