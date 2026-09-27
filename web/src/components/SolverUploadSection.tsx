import { Alert, Badge, Button, Divider, FileInput, Group, Paper, Stack, Text, Title } from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiChevronDown, mdiChevronUp, mdiCodeBraces, mdiFileCodeOutline, mdiLockOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import localizedFormat from 'dayjs/plugin/localizedFormat'
import { FC, FormEvent, useId, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { BlobUploadOperation, retainBlobUploadOperation } from '@Utils/BlobUploadOperations'
import { httpErrorStatus } from '@Utils/HttpError'
import { HunamizeSize, showErrorMsg } from '@Utils/Shared'
import api from '@Api'

dayjs.extend(localizedFormat)

export interface SolverUploadSectionProps {
  gameId: number
  challengeId: number
}

/** One non-polled read per mount; uploads reconcile through the same cache entry. */
const SOLVER_READ_CONFIG = {
  refreshInterval: 0,
  revalidateOnFocus: false,
  revalidateOnReconnect: false,
  shouldRetryOnError: false,
}

/** Optional solver upload on a solved Jeopardy challenge. Never blocks closing the card. */
export const SolverUploadSection: FC<SolverUploadSectionProps> = ({ gameId, challengeId }) => {
  const { t } = useTranslation()
  const headingId = useId()
  const panelId = useId()
  const [expanded, setExpanded] = useState(false)
  const [file, setFile] = useState<File | null>(null)
  const [uploading, setUploading] = useState(false)
  const operation = useRef<BlobUploadOperation | null>(null)

  const {
    data: state,
    error,
    isLoading,
    mutate,
  } = api.game.useGameGetSolverUploads(gameId, challengeId, SOLVER_READ_CONFIG, gameId > 0 && challengeId > 0)

  // A disabled event (404) or a non-participant (400/403) has nothing to show here.
  const hiddenStatus = httpErrorStatus(error)
  if (hiddenStatus === 404 || hiddenStatus === 400 || hiddenStatus === 403) return null

  const versions = state?.versions ?? []
  const editable = Boolean(state?.editable)
  const tooLarge = file !== null && state !== undefined && file.size > state.maxFileBytes
  const fileError = tooLarge
    ? t('challenge.solver.too_large', 'The file must be {{size}} or smaller.', {
        size: HunamizeSize(state.maxFileBytes),
      })
    : file?.size === 0
      ? t('challenge.solver.empty', 'The file is empty.')
      : undefined

  const onUpload = async (event: FormEvent) => {
    event.preventDefault()
    if (!file || !editable || fileError || uploading) return
    setUploading(true)
    try {
      operation.current = retainBlobUploadOperation(operation.current, file)
      const { data } = await api.game.gameSubmitSolverUpload(gameId, challengeId, { file }, operation.current.id)
      operation.current = null
      setFile(null)
      await mutate(data, { revalidate: false })
      showNotification({
        color: 'teal',
        message: t('challenge.solver.uploaded', 'Solver uploaded'),
        icon: <Icon path={mdiCheck} size={1} aria-hidden="true" />,
      })
    } catch (uploadError) {
      showErrorMsg(uploadError, t)
    } finally {
      setUploading(false)
    }
  }

  const summary = !state
    ? isLoading
      ? t('challenge.solver.loading', 'Loading…')
      : t('challenge.solver.load_failed', 'Solver uploads could not be loaded.')
    : versions.length === 0
      ? t('challenge.solver.none', 'Optional')
      : t('challenge.solver.count', '{{count}} of {{max}} versions', {
          count: versions.length,
          max: state.maxVersions,
        })

  return (
    <Stack gap="xs" component="section" aria-labelledby={headingId} data-solver-uploads>
      <Divider />
      <Group justify="space-between" gap="xs" wrap="wrap">
        <Group gap={6} wrap="nowrap" style={{ minWidth: 0 }}>
          <Icon path={mdiCodeBraces} size={0.8} aria-hidden="true" />
          <Title order={3} size="h5" id={headingId}>
            {t('challenge.solver.title', 'Solver')}
          </Title>
          <Text size="xs" c="dimmed">
            {summary}
          </Text>
        </Group>
        <Button
          size="compact-sm"
          variant="subtle"
          aria-expanded={expanded}
          aria-controls={panelId}
          disabled={!state}
          rightSection={<Icon path={expanded ? mdiChevronUp : mdiChevronDown} size={0.7} aria-hidden="true" />}
          onClick={() => setExpanded(!expanded)}
        >
          {expanded
            ? t('challenge.solver.hide', 'Hide')
            : editable
              ? t('challenge.solver.manage', 'Upload')
              : t('challenge.solver.show', 'Show')}
        </Button>
      </Group>
      {!state && !isLoading && (
        <Group gap="xs">
          <Button size="compact-xs" variant="default" onClick={() => void mutate()}>
            {t('common.button.retry', 'Retry')}
          </Button>
        </Group>
      )}
      {state && expanded && (
        <Stack gap="xs" id={panelId}>
          <Text size="xs" c="dimmed">
            {t(
              'challenge.solver.description',
              'Optionally upload the script or notes your team used to solve this challenge so organizers can verify the solve. Each upload is kept as a new version; organizers can see every version and when it was uploaded.'
            )}
          </Text>
          {!editable && (
            <Alert color="gray" p="xs" icon={<Icon path={mdiLockOutline} size={0.8} aria-hidden="true" />}>
              <Text size="xs">
                {!state.solved
                  ? t('challenge.solver.unsolved', 'Solve this challenge to upload a solver.')
                  : versions.length >= state.maxVersions
                    ? t('challenge.solver.full', 'Your team has uploaded the maximum number of versions.')
                    : t('challenge.solver.closed', 'The window for uploading solvers has closed.')}
              </Text>
            </Alert>
          )}
          {editable && (
            <form onSubmit={(event) => void onUpload(event)}>
              <Stack gap="xs">
                <FileInput
                  label={t('challenge.solver.select_label', 'Solver file')}
                  placeholder={t('challenge.solver.select_placeholder', 'Choose a file')}
                  description={t(
                    'challenge.solver.limits',
                    'Up to {{size}} per file. Your team has used {{used}} of {{limit}} for this event.',
                    {
                      size: HunamizeSize(state.maxFileBytes),
                      used: HunamizeSize(state.teamBytesUsed),
                      limit: HunamizeSize(state.teamBytesLimit),
                    }
                  )}
                  value={file}
                  onChange={(next) => {
                    setFile(next)
                    if (!next) operation.current = null
                  }}
                  error={fileError}
                  disabled={uploading}
                  clearable
                  clearButtonProps={{ 'aria-label': t('challenge.solver.clear_file', 'Remove selected file') }}
                  leftSection={<Icon path={mdiFileCodeOutline} size={0.8} aria-hidden="true" />}
                />
                <Group justify="flex-end">
                  <Button type="submit" size="compact-sm" loading={uploading} disabled={!file || Boolean(fileError)}>
                    {t('challenge.solver.upload', 'Upload solver')}
                  </Button>
                </Group>
              </Stack>
            </form>
          )}
          {versions.length === 0 ? (
            <Text size="sm" c="dimmed">
              {t('challenge.solver.empty_list', 'No solver uploaded yet.')}
            </Text>
          ) : (
            <Stack gap={6} component="ul" m={0} p={0} style={{ listStyle: 'none' }}>
              {versions.map((version) => (
                <Paper key={version.id} component="li" withBorder p="xs" radius="sm">
                  <Group gap={6} wrap="wrap">
                    <Badge size="sm" variant="light" tt="none">
                      {t('challenge.solver.version', 'v{{version}}', { version: version.version })}
                    </Badge>
                    <Text size="sm" fw={600} style={{ overflowWrap: 'anywhere', minWidth: 0 }}>
                      {version.fileName}
                    </Text>
                    <Text size="xs" c="dimmed">
                      {HunamizeSize(version.sizeBytes)}
                    </Text>
                  </Group>
                  <Text size="xs" c="dimmed">
                    {t('challenge.solver.uploaded_by', 'Uploaded by {{user}} at {{time}}', {
                      user: version.uploadedBy ?? '—',
                      time: dayjs(version.uploadedAt).format('L LT'),
                    })}
                  </Text>
                  <Text size="xs" c="dimmed" ff="monospace" style={{ overflowWrap: 'anywhere' }}>
                    SHA-256 {version.sha256.slice(0, 16)}…
                  </Text>
                </Paper>
              ))}
            </Stack>
          )}
        </Stack>
      )}
    </Stack>
  )
}
