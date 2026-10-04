import {
  Alert,
  Badge,
  Button,
  Center,
  FileInput,
  Group,
  Loader,
  Modal,
  ModalProps,
  Paper,
  Progress,
  Stack,
  Text,
} from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiAlertCircleOutline, mdiCheck, mdiFileDocumentOutline, mdiUpload } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Markdown } from '@Components/MarkdownRenderer'
import { BlobUploadOperation, retainBlobUploadOperation } from '@Utils/BlobUploadOperations'
import { useLanguage } from '@Utils/I18n'
import { useServerNow } from '@Utils/ServerClock'
import { HunamizeSize, tryGetErrorMsg } from '@Utils/Shared'
import {
  isWriteupDeadlineError,
  writeupFailureKind,
  writeupFileProblem,
  writeupUploadPercent,
} from '@Utils/WriteupUpload'
import { OnceSWRConfig } from '@Hooks/useConfig'
import api from '@Api'
import classes from '@Styles/WriteupSubmitModal.module.css'

export { isWriteupDeadlineError } from '@Utils/WriteupUpload'

interface WriteupSubmitModalProps extends ModalProps {
  gameId: number
  writeupDeadline: number
}

export const WriteupSubmitModal: FC<WriteupSubmitModalProps> = ({ gameId, writeupDeadline: wpddl, ...props }) => {
  const { data, error, mutate, isValidating } = api.game.useGameGetWriteup(
    gameId,
    { ...OnceSWRConfig, shouldRetryOnError: false },
    Boolean(props.opened)
  )
  const { t } = useTranslation()
  const { locale } = useLanguage()
  const ddl = useMemo(() => dayjs(wpddl), [wpddl])
  const now = useServerNow()
  const [file, setFile] = useState<File | null>(null)
  const [uploading, setUploading] = useState(false)
  const [deadlineRejected, setDeadlineRejected] = useState(false)
  const [progress, setProgress] = useState<number | null>(null)
  const [uploadError, setUploadError] = useState<string | null>(null)
  const [saved, setSaved] = useState<Pick<File, 'name' | 'size'> | null>(null)
  const uploadOperation = useRef<BlobUploadOperation | null>(null)
  const activeRequest = useRef<AbortController | null>(null)
  const deadlinePassed = !ddl.isValid() || now.isAfter(ddl) || deadlineRejected
  const disabled = uploading || deadlinePassed
  const problem = file ? writeupFileProblem(file) : null
  const fileError =
    problem === 'format'
      ? t('game.content.writeup.file_format_error', 'Choose a PDF file with a .pdf extension.')
      : problem === 'empty'
        ? t('game.content.writeup.file_empty_error', 'This file is empty. Choose your completed writeup.')
        : problem === 'size'
          ? t('game.content.writeup.file_size_error', 'The PDF must be 20 MiB or smaller.')
          : undefined

  useEffect(() => {
    setFile(null)
    setDeadlineRejected(false)
    setUploading(false)
    setProgress(null)
    setUploadError(null)
    setSaved(null)
    uploadOperation.current = null
    return () => {
      activeRequest.current?.abort()
      activeRequest.current = null
    }
  }, [gameId, wpddl])

  useEffect(() => {
    if (!uploading) return
    const warnBeforeLeaving = (event: BeforeUnloadEvent) => {
      event.preventDefault()
      event.returnValue = ''
    }
    window.addEventListener('beforeunload', warnBeforeLeaving)
    return () => window.removeEventListener('beforeunload', warnBeforeLeaving)
  }, [uploading])

  const failureMessage = (err: unknown) => {
    switch (writeupFailureKind(err)) {
      case 'deadline':
        return t('game.content.writeup.deadline_exceeded')
      case 'session':
        return t(
          'game.content.writeup.session_error',
          'Your session expired. Sign in again, then check your submission status before retrying.'
        )
      case 'size':
        return t('game.content.writeup.file_size_error', 'The PDF must be 20 MiB or smaller.')
      case 'busy':
        return t(
          'game.content.writeup.busy_error',
          'Uploads are busy right now. Your selected file is kept here; wait a moment, then retry.'
        )
      case 'connection':
        return t(
          'game.content.writeup.connection_error',
          'We could not confirm that your writeup was saved. Check its status, then retry with the selected file if needed.'
        )
      default:
        return tryGetErrorMsg(err, t)
    }
  }

  const onUpload = async () => {
    if (!file || !data || disabled || problem || activeRequest.current) return
    const controller = new AbortController()
    activeRequest.current = controller
    setProgress(null)
    setUploadError(null)
    setUploading(true)
    try {
      uploadOperation.current = retainBlobUploadOperation(uploadOperation.current, file)
      await api.game.gameSubmitWriteup(gameId, { file }, uploadOperation.current.id, {
        signal: controller.signal,
        onUploadProgress: (event) => {
          if (activeRequest.current === controller) setProgress(writeupUploadPercent(event.loaded, event.total))
        },
      })
      if (activeRequest.current !== controller) return
      uploadOperation.current = null
      setFile(null)
      setSaved({ name: file.name, size: file.size })
      showNotification({
        color: 'teal',
        message: t('game.notification.writeup.submitted'),
        icon: <Icon path={mdiCheck} size={1} aria-hidden="true" />,
      })
      // A failed status refresh must not turn an acknowledged upload into a failed upload.
      void mutate().catch(() => undefined)
    } catch (err) {
      if (activeRequest.current !== controller) return
      if (isWriteupDeadlineError(err)) setDeadlineRejected(true)
      setUploadError(failureMessage(err))
    } finally {
      if (activeRequest.current === controller) {
        activeRequest.current = null
        setProgress(null)
        setUploading(false)
      }
    }
  }

  return (
    <Modal
      {...props}
      title={t('game.content.writeup.title')}
      size="lg"
      closeOnClickOutside={!uploading}
      closeButtonProps={{
        ...props.closeButtonProps,
        'aria-label': props.closeButtonProps?.['aria-label'] ?? t('common.button.close', 'Close'),
      }}
      classNames={{ title: classes.modalTitle }}
    >
      <Stack gap="lg">
        {error && (
          <Alert
            color="red"
            role="alert"
            icon={<Icon path={mdiAlertCircleOutline} size={0.9} aria-hidden="true" />}
            title={t('game.content.writeup.load_failed', 'Writeup status could not be loaded')}
          >
            <Text size="sm">
              {t(
                'game.content.writeup.status_retry_hint',
                'Check your connection and refresh the status. Your selected file will stay here.'
              )}
            </Text>
            <Button
              mt="sm"
              variant="outline"
              loading={isValidating}
              onClick={() => void mutate().catch(() => undefined)}
            >
              {t('common.button.retry', 'Retry')}
            </Button>
          </Alert>
        )}
        {!data && !error && (
          <Center py="xl" role="status" aria-live="polite">
            <Loader aria-label={t('common.content.loading', 'Loading')} />
          </Center>
        )}
        {(data || file || saved) && (
          <>
            <Paper withBorder p="md" radius="md" className={classes.summary}>
              <Stack gap="sm">
                <Group justify="space-between" gap="sm">
                  <Text fw={600}>{t('game.content.writeup.current')}</Text>
                  <Badge variant="light" color={data?.submitted || saved ? 'teal' : 'gray'} className={classes.status}>
                    {data?.submitted || saved
                      ? t('game.content.writeup.submitted')
                      : t('game.content.writeup.unsubmitted')}
                  </Badge>
                </Group>
                {data?.submitted || saved ? (
                  <Group wrap="nowrap" gap="sm" align="flex-start">
                    <Icon path={mdiFileDocumentOutline} size={1.25} aria-hidden="true" className={classes.fileIcon} />
                    <Stack gap={2} miw={0}>
                      <Text fw={500} className={classes.fileName}>
                        {saved?.name ?? data?.name}
                      </Text>
                      <Text size="sm" c="dimmed">
                        {HunamizeSize(saved?.size ?? data?.fileSize ?? 0)}
                      </Text>
                    </Stack>
                  </Group>
                ) : (
                  !saved && (
                    <Text size="sm" c="dimmed">
                      {t('game.content.writeup.unsubmitted_note')}
                    </Text>
                  )
                )}
              </Stack>
            </Paper>
            <Stack gap={4}>
              <Text size="sm" fw={600}>
                {t('game.content.writeup.deadline_label', 'Submission deadline')}
              </Text>
              <Text size="sm" className={classes.deadline}>
                {ddl.isValid() ? (
                  <time dateTime={ddl.toISOString()}>
                    {ddl.locale(locale).format('LL LTS')} (UTC{ddl.format('Z')})
                  </time>
                ) : (
                  '—'
                )}
              </Text>
              <Text size="sm" c="dimmed">
                {t(
                  'game.content.writeup.requirements',
                  'Combine the writeups for all solved challenges into one PDF, up to 20 MiB.'
                )}
              </Text>
            </Stack>
            {data?.note && (
              <section aria-label={t('game.content.writeup.instructions.additional')}>
                <Markdown source={data.note} />
              </section>
            )}
            {deadlinePassed && (
              <Alert color="yellow" role="status">
                {t('game.content.writeup.deadline_exceeded')}
              </Alert>
            )}
            {saved && !uploading && !file && (
              <Alert color="teal" role="status" icon={<Icon path={mdiCheck} size={0.9} aria-hidden="true" />}>
                {t(
                  'game.content.writeup.saved_confirmation',
                  'Your writeup has been saved. You can close this window.'
                )}
              </Alert>
            )}
            <form
              onSubmit={(event) => {
                event.preventDefault()
                void onUpload()
              }}
            >
              <Stack gap="md">
                <FileInput
                  label={t('game.content.writeup.select_label', 'Writeup PDF')}
                  placeholder={t('game.content.writeup.select_placeholder', 'Choose your PDF')}
                  description={t(
                    'game.content.writeup.selection_hint',
                    'Choose a file, then submit it below. Selecting a file does not upload it.'
                  )}
                  value={file}
                  onChange={(next) => {
                    setFile(next)
                    setUploadError(null)
                    if (!next) uploadOperation.current = null
                  }}
                  accept="application/pdf,.pdf"
                  disabled={disabled}
                  error={fileError}
                  clearable
                  clearButtonProps={{ 'aria-label': t('game.content.writeup.clear_file', 'Remove selected PDF') }}
                  leftSection={<Icon path={mdiFileDocumentOutline} size={0.9} aria-hidden="true" />}
                  classNames={{ input: classes.fileInput }}
                />
                {file && (
                  <Text size="sm" c="dimmed" className={classes.fileName}>
                    {file.name} · {HunamizeSize(file.size)}
                  </Text>
                )}
                {(data?.submitted || saved) && file && !problem && (
                  <Text size="sm" c="dimmed">
                    {t(
                      'game.content.writeup.replace_hint',
                      'A successful upload replaces your team’s current writeup.'
                    )}
                  </Text>
                )}
                {uploadError && (
                  <Alert
                    color="red"
                    role="alert"
                    title={t('game.content.writeup.upload_failed', 'Upload not confirmed')}
                  >
                    {uploadError}
                    <Button
                      mt="sm"
                      variant="outline"
                      size="xs"
                      loading={isValidating}
                      onClick={() => void mutate().catch(() => undefined)}
                    >
                      {t('game.content.writeup.check_status', 'Check submission status')}
                    </Button>
                  </Alert>
                )}
                {uploading && (
                  <Stack gap="xs">
                    <Group justify="space-between" gap="xs" role="status" aria-live="polite">
                      <Text size="sm">
                        {progress === 100
                          ? t('game.content.writeup.saving', 'Saving your writeup…')
                          : t('game.button.writeup.uploading')}
                      </Text>
                      {progress !== null && (
                        <Text size="sm" ff="monospace">
                          {progress}%
                        </Text>
                      )}
                    </Group>
                    {progress !== null && <Progress value={progress} aria-label={t('game.button.writeup.uploading')} />}
                    <Text size="xs" c="dimmed">
                      {t(
                        'game.content.writeup.keep_open',
                        'Keep this page open until saving is confirmed. A slow connection may take a few minutes.'
                      )}
                    </Text>
                  </Stack>
                )}
                <Button
                  type="submit"
                  fullWidth
                  disabled={disabled || !file || Boolean(problem) || !data}
                  loading={uploading}
                  leftSection={<Icon path={mdiUpload} size={0.9} aria-hidden="true" />}
                >
                  {deadlinePassed
                    ? t('game.content.writeup.deadline_exceeded')
                    : uploadError
                      ? t('game.content.writeup.retry_upload', 'Retry upload')
                      : t('game.button.writeup.upload')}
                </Button>
              </Stack>
            </form>
          </>
        )}
      </Stack>
    </Modal>
  )
}
