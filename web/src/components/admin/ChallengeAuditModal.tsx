import {
  Anchor,
  Badge,
  Button,
  Center,
  Code,
  Divider,
  Group,
  Loader,
  Modal,
  ModalProps,
  Paper,
  ScrollArea,
  Stack,
  Text,
  Title,
} from '@mantine/core'
import { mdiDownload, mdiFileDocumentOutline, mdiFolderZipOutline, mdiHammerWrench } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { CompletionPollSWRConfig, useCompletionPolling } from '@Hooks/useCompletionPolling'
import { HunamizeSize, showErrorMsg } from '@Utils/Shared'
import { createOperationId, startControlJob, waitForControlJob } from '@Utils/ControlJobs'
import api, { ChallengeAuditModel } from '@Api'

const AUDIT_PROJECTION_CACHE_LIMIT = 16
const auditProjectionCache = new Map<string, ChallengeAuditModel>()

const cacheAuditProjection = (key: string, projection: ChallengeAuditModel) => {
  auditProjectionCache.delete(key)
  auditProjectionCache.set(key, projection)
  while (auditProjectionCache.size > AUDIT_PROJECTION_CACHE_LIMIT) {
    const oldest = auditProjectionCache.keys().next().value
    if (oldest === undefined) break
    auditProjectionCache.delete(oldest)
  }
}

const pageCanInspectArchive = () =>
  (typeof document === 'undefined' || document.visibilityState !== 'hidden') &&
  (typeof navigator === 'undefined' || navigator.onLine !== false)

interface ChallengeAuditModalProps extends Omit<ModalProps, 'children'> {
  gameId: number
  challengeId: number | null
  challengeTitle?: string
  submitter?: string | null
}

export const ChallengeAuditModal: FC<ChallengeAuditModalProps> = (props) => {
  const { gameId, challengeId, challengeTitle, submitter, opened, onClose, ...rest } = props
  const { t } = useTranslation()
  const [audit, setAudit] = useState<ChallengeAuditModel | null>(null)
  const [loading, setLoading] = useState(false)
  const buildFlight = useRef<Promise<void> | null>(null)
  const buildAbort = useRef(new AbortController())
  const statusQuery = api.edit.useEditGetChallengeBuildStatus(
    gameId,
    challengeId ?? -1,
    CompletionPollSWRConfig,
    opened && challengeId != null
  )
  const latestStatus = useRef(statusQuery.data)
  latestStatus.current = statusQuery.data
  const archiveAvailable = statusQuery.data?.archiveAvailable
  const archiveVersion = statusQuery.data?.archiveVersion
  const inFlight = statusQuery.data?.buildStatus === 'Queued' || statusQuery.data?.buildStatus === 'Building'

  useCompletionPolling({
    key:
      opened && challengeId != null && inFlight
        ? `/api/edit/games/${gameId}/challenges/${challengeId}/buildstatus`
        : '',
    phase: `audit:${gameId}:${challengeId ?? 'closed'}`,
    enabled: opened && challengeId != null && inFlight,
    data: statusQuery.data,
    error: statusQuery.error,
    isValidating: statusQuery.isValidating,
    mutate: statusQuery.mutate,
    successDelay: () => 2_000,
  })

  useEffect(() => {
    if (!opened || challengeId == null) {
      setAudit(null)
      setLoading(false)
      return
    }
    const status = latestStatus.current
    if (!status) return
    if (!status.archiveAvailable || !status.archiveVersion) {
      setAudit({
        archiveAvailable: false,
        files: [],
        previews: {},
        yamlText: null,
        buildStatus: status.buildStatus,
        lastBuildLog: status.lastBuildLog,
      })
      setLoading(false)
      return
    }

    const cacheKey = `${gameId}:${challengeId}:${status.archiveVersion}`
    const cached = auditProjectionCache.get(cacheKey)
    if (cached) {
      setAudit({ ...cached, buildStatus: status.buildStatus, lastBuildLog: status.lastBuildLog })
      setLoading(false)
      return
    }

    let controller: AbortController | null = null
    let cancelled = false
    const load = async () => {
      if (!pageCanInspectArchive() || controller) return
      const requestController = new AbortController()
      controller = requestController
      setLoading(true)
      try {
        const res = await api.edit.editGetChallengeAuditMeta(gameId, challengeId, {
          signal: requestController.signal,
        })
        if (cancelled) return
        const projection = { ...res.data, buildStatus: undefined, lastBuildLog: undefined }
        cacheAuditProjection(cacheKey, projection)
        const currentStatus = latestStatus.current ?? status
        setAudit({
          ...projection,
          buildStatus: currentStatus.buildStatus,
          lastBuildLog: currentStatus.lastBuildLog,
        })
      } catch (e) {
        if (!cancelled && !requestController.signal.aborted) {
          setAudit(null)
          showErrorMsg(e, t)
        }
      } finally {
        if (controller === requestController) controller = null
        if (!cancelled) setLoading(false)
      }
    }
    const updateActivity = () => {
      if (!pageCanInspectArchive()) {
        controller?.abort()
        controller = null
        setLoading(false)
        return
      }
      void load()
    }
    document.addEventListener('visibilitychange', updateActivity)
    window.addEventListener('online', updateActivity)
    window.addEventListener('offline', updateActivity)
    void load()
    return () => {
      cancelled = true
      controller?.abort()
      document.removeEventListener('visibilitychange', updateActivity)
      window.removeEventListener('online', updateActivity)
      window.removeEventListener('offline', updateActivity)
    }
  }, [opened, gameId, challengeId, archiveAvailable, archiveVersion, t])

  useEffect(() => {
    const status = statusQuery.data
    if (!status) return
    setAudit((current) =>
      current ? { ...current, buildStatus: status.buildStatus, lastBuildLog: status.lastBuildLog } : current
    )
  }, [statusQuery.data])

  useEffect(() => {
    if (opened) {
      if (buildAbort.current.signal.aborted) buildAbort.current = new AbortController()
      return
    }
    buildAbort.current.abort()
  }, [opened])

  useEffect(() => () => buildAbort.current.abort(), [])

  const downloadArchive = () => {
    if (challengeId == null) return
    window.open(`/api/edit/games/${gameId}/challenges/${challengeId}/auditarchive`, '_blank', 'noopener,noreferrer')
  }

  const [rebuilding, setRebuilding] = useState(false)
  const onRebuild = () => {
    if (challengeId == null) return Promise.resolve()
    if (buildFlight.current) return buildFlight.current
    const targetChallengeId = challengeId
    const operationId = createOperationId()
    const task = (async () => {
      setRebuilding(true)
      try {
        const job = await startControlJob(
          operationId,
          () =>
            api.edit.editRebuildChallengeImage(gameId, targetChallengeId, operationId, {
              signal: buildAbort.current.signal,
            }),
          buildAbort.current.signal
        )
        await statusQuery.mutate()
        await waitForControlJob(job, buildAbort.current.signal)
        await statusQuery.mutate()
      } catch (e) {
        if (!(e instanceof DOMException && e.name === 'AbortError')) showErrorMsg(e, t)
      } finally {
        setRebuilding(false)
        buildFlight.current = null
      }
    })()
    buildFlight.current = task
    return task
  }

  return (
    <Modal
      size="xl"
      opened={opened}
      onClose={onClose}
      title={
        <Group gap="sm">
          <Icon path={mdiFolderZipOutline} size={1} />
          <Stack gap={0}>
            <Title order={4}>{t('admin.content.audit.title')}</Title>
            {challengeTitle && (
              <Text size="xs" c="dimmed">
                {challengeTitle}
                {submitter ? ` — ${submitter}` : ''}
              </Text>
            )}
          </Stack>
        </Group>
      }
      {...rest}
    >
      {loading || statusQuery.isLoading ? (
        <Center py="xl">
          <Loader />
        </Center>
      ) : !audit ? (
        <Center py="xl">
          <Text c="dimmed">{t('admin.content.audit.unavailable')}</Text>
        </Center>
      ) : (
        <Stack gap="md">
          {audit.archiveAvailable ? (
            <Group justify="space-between">
              <Text size="sm" c="dimmed">
                {t('admin.content.audit.archive_available')}
              </Text>
              <Group gap="xs">
                {audit.buildStatus && audit.buildStatus !== 'None' && (
                  <Button
                    size="xs"
                    variant="default"
                    leftSection={<Icon path={mdiHammerWrench} size={0.9} />}
                    loading={rebuilding}
                    onClick={onRebuild}
                  >
                    {t('admin.button.audit.rebuild')}
                  </Button>
                )}
                <Button size="xs" leftSection={<Icon path={mdiDownload} size={0.9} />} onClick={downloadArchive}>
                  {t('admin.button.audit.download')}
                </Button>
              </Group>
            </Group>
          ) : (
            <Text size="sm" c="dimmed">
              {t('admin.content.audit.no_archive')}
            </Text>
          )}

          {audit.buildStatus && audit.buildStatus !== 'None' && (
            <Paper p="sm" withBorder>
              <Stack gap={4}>
                <Group gap="xs">
                  <Title order={6}>{t('admin.content.audit.build_log')}</Title>
                  <Badge
                    size="xs"
                    color={
                      audit.buildStatus === 'Success'
                        ? 'teal'
                        : audit.buildStatus === 'Failed'
                          ? 'red'
                          : audit.buildStatus === 'NotApplicable'
                            ? 'gray'
                            : audit.buildStatus === 'MissingDockerfile'
                              ? 'orange'
                              : audit.buildStatus === 'Queued'
                                ? 'blue'
                                : 'yellow'
                    }
                    variant={audit.buildStatus === 'Failed' ? 'filled' : 'light'}
                  >
                    {audit.buildStatus}
                  </Badge>
                </Group>
                {audit.lastBuildLog ? (
                  <Code
                    block
                    style={{
                      whiteSpace: 'pre-wrap',
                      maxHeight: '30vh',
                      overflowY: 'auto',
                      fontSize: 11,
                    }}
                  >
                    {audit.lastBuildLog}
                  </Code>
                ) : (
                  <Text size="xs" c="dimmed">
                    {t('admin.content.audit.no_build_log')}
                  </Text>
                )}
              </Stack>
            </Paper>
          )}

          <Divider />

          <Stack gap={6}>
            <Title order={5}>{t('admin.content.audit.yaml')}</Title>
            {audit.yamlText ? (
              <Code
                block
                style={{
                  whiteSpace: 'pre-wrap',
                  maxHeight: '40vh',
                  overflowY: 'auto',
                  fontSize: 12,
                }}
              >
                {audit.yamlText}
              </Code>
            ) : (
              <Text size="sm" c="dimmed">
                {t('admin.content.audit.no_yaml')}
              </Text>
            )}
          </Stack>

          <Divider />

          <Stack gap={6}>
            <Title order={5}>
              {t('admin.content.audit.files')}{' '}
              <Text span size="sm" c="dimmed">
                ({audit.files.length})
              </Text>
            </Title>
            <ScrollArea h={Math.min(audit.files.length * 26 + 12, 240)} type="auto">
              <Stack gap={2}>
                {audit.files.map((f) => (
                  <Group key={f.path} gap="xs" wrap="nowrap" justify="space-between">
                    <Group gap={4} wrap="nowrap" miw={0}>
                      <Icon path={mdiFileDocumentOutline} size={0.7} />
                      <Anchor
                        component="span"
                        size="sm"
                        ff="monospace"
                        truncate
                        c={Object.keys(audit.previews).includes(f.path) ? 'blue' : undefined}
                      >
                        {f.path}
                      </Anchor>
                    </Group>
                    <Badge size="xs" variant="light" color="gray">
                      {HunamizeSize(f.size)}
                    </Badge>
                  </Group>
                ))}
              </Stack>
            </ScrollArea>
          </Stack>

          {Object.keys(audit.previews).length > 0 && (
            <>
              <Divider />
              <Stack gap="sm">
                <Title order={5}>{t('admin.content.audit.previews')}</Title>
                {Object.entries(audit.previews).map(([path, contents]) => (
                  <Paper key={path} p="sm" withBorder>
                    <Stack gap={4}>
                      <Text size="xs" ff="monospace" fw="bold">
                        {path}
                      </Text>
                      <Code
                        block
                        style={{
                          whiteSpace: 'pre-wrap',
                          maxHeight: '30vh',
                          overflowY: 'auto',
                          fontSize: 12,
                        }}
                      >
                        {contents}
                      </Code>
                    </Stack>
                  </Paper>
                ))}
              </Stack>
            </>
          )}
        </Stack>
      )}
    </Modal>
  )
}
