import {
  Alert,
  Button,
  Card,
  Center,
  Divider,
  FileButton,
  Group,
  List,
  Loader,
  Modal,
  ModalProps,
  Progress,
  Stack,
  Text,
  Title,
  alpha,
  useMantineTheme,
} from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiAlertCircleOutline, mdiCheck, mdiExclamationThick, mdiFileDocumentOutline, mdiFileHidden } from '@mdi/js'
import { Icon } from '@mdi/react'
import cx from 'clsx'
import dayjs from 'dayjs'
import { FC, useEffect, useMemo, useRef, useState } from 'react'
import { Trans, useTranslation } from 'react-i18next'
import { Markdown } from '@Components/MarkdownRenderer'
import { BLOB_OPERATION_HEADER, BlobUploadOperation, retainBlobUploadOperation } from '@Utils/BlobUploadOperations'
import { useLanguage } from '@Utils/I18n'
import { useServerNow } from '@Utils/ServerClock'
import { showErrorMsg } from '@Utils/Shared'
import { HunamizeSize } from '@Utils/Shared'
import { OnceSWRConfig } from '@Hooks/useConfig'
import api from '@Api'
import misc from '@Styles/Misc.module.css'
import uploadClasses from '@Styles/Upload.module.css'

interface WriteupSubmitModalProps extends ModalProps {
  gameId: number
  writeupDeadline: number
}

export const WriteupSubmitModal: FC<WriteupSubmitModalProps> = ({ gameId, writeupDeadline: wpddl, ...props }) => {
  const opened = Boolean(props.opened)
  const { data, error, mutate } = api.game.useGameGetWriteup(
    gameId,
    { ...OnceSWRConfig, shouldRetryOnError: false },
    opened
  )

  const theme = useMantineTheme()
  const ddl = useMemo(() => dayjs(wpddl), [wpddl])
  const now = useServerNow()
  const { locale } = useLanguage()
  const [uploading, setUploading] = useState(false)
  const [deadlineRejected, setDeadlineRejected] = useState(false)
  const [progress, setProgress] = useState(0)
  const uploadOperation = useRef<BlobUploadOperation | null>(null)
  const noteColor = data?.submitted ? theme.colors.teal[5] : theme.colors.red[5]
  const deadlinePassed = !ddl.isValid() || now.isAfter(ddl)
  const disabled = uploading || deadlinePassed || deadlineRejected

  const { t } = useTranslation()

  useEffect(() => {
    setDeadlineRejected(false)
    setUploading(false)
    setProgress(0)
    uploadOperation.current = null
  }, [gameId, wpddl])

  const onUpload = async (file: File | null) => {
    if (!file || disabled) return

    setProgress(0)
    setUploading(true)

    try {
      uploadOperation.current = retainBlobUploadOperation(uploadOperation.current, file)
      await api.game.gameSubmitWriteup(
        gameId,
        {
          file,
        },
        {
          headers: { [BLOB_OPERATION_HEADER]: uploadOperation.current.id },
          onUploadProgress: (e) => {
            setProgress((e.loaded / (e.total ?? 1)) * 100)
          },
        }
      )
      uploadOperation.current = null
      setProgress(100)
      showNotification({
        color: 'teal',
        message: t('game.notification.writeup.submitted'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      mutate()
    } catch (err) {
      if (isWriteupDeadlineError(err) || now.isAfter(ddl)) setDeadlineRejected(true)
      showErrorMsg(err, t)
    } finally {
      setProgress(0)
      setUploading(false)
    }
  }

  return (
    <Modal
      title={
        <Group w="100%" justify="space-between">
          <Title order={4}>{t('game.content.writeup.title')}</Title>
          {data && (
            <Group gap={4}>
              <Icon path={data.submitted ? mdiCheck : mdiExclamationThick} size={0.9} color={noteColor} />
              <Text fw={600} size="md" c={noteColor}>
                {data.submitted ? t('game.content.writeup.submitted') : t('game.content.writeup.unsubmitted')}
              </Text>
            </Group>
          )}
        </Group>
      }
      {...props}
      classNames={{
        header: misc.m0,
        title: cx(misc.w100, misc.m0),
      }}
    >
      {error ? (
        <Alert
          color="red"
          icon={<Icon path={mdiAlertCircleOutline} size={0.9} aria-hidden="true" />}
          title={t('game.content.writeup.load_failed', 'Writeup status could not be loaded')}
          role="alert"
        >
          <Button mt="sm" variant="outline" onClick={() => void mutate()}>
            {t('common.button.retry', 'Retry')}
          </Button>
        </Alert>
      ) : !data ? (
        <Center py="xl" role="status" aria-live="polite">
          <Loader aria-label={t('common.content.loading', 'Loading')} />
        </Center>
      ) : (
        <Stack gap="xs" mt={0}>
          <Divider />
          <Title order={5}>{t('game.content.writeup.instructions.title')}</Title>
          <List classNames={{ itemWrapper: misc.listItemWrapper }}>
            <List.Item>
              <Text>
                <Trans
                  i18nKey="game.content.writeup.instructions.deadline"
                  values={{
                    datetime: ddl.locale(locale).format('LL LTS'),
                  }}
                >
                  _
                  <Text mx={5} span fw={600} c="yellow">
                    _
                  </Text>
                  _
                </Trans>
              </Text>
            </List.Item>
            <List.Item>
              <Text>
                <Trans i18nKey="game.content.writeup.instructions.file_format">
                  _
                  <Text mx={5} fw={600} span c="yellow">
                    _
                  </Text>
                  _
                </Trans>
              </Text>
            </List.Item>
          </List>
          {data?.note && (
            <>
              <Title order={5}>{t('game.content.writeup.instructions.additional')}</Title>
              <Markdown source={data.note} />
            </>
          )}
          <Title order={5}>{t('game.content.writeup.current')}</Title>
          <Card>
            {data && data.submitted ? (
              <Group>
                <Icon path={mdiFileDocumentOutline} size={1.5} />
                <Stack gap={0}>
                  <Text fw={600} size="md">
                    {data.name}
                  </Text>
                  <Text fw={600} size="sm" c="dimmed" ff="monospace">
                    {data.fileSize && HunamizeSize(data.fileSize)}
                  </Text>
                </Stack>
              </Group>
            ) : (
              <Group>
                <Icon path={mdiFileHidden} size={1.5} />
                <Stack gap={0}>
                  <Text fw={600} size="md">
                    {t('game.content.writeup.unsubmitted_note')}
                  </Text>
                </Stack>
              </Group>
            )}
          </Card>
          <FileButton onChange={onUpload} accept="application/pdf">
            {(props) => (
              <Button
                {...props}
                fullWidth
                className={uploadClasses.button}
                disabled={disabled}
                color={progress !== 0 ? 'cyan' : theme.primaryColor}
              >
                <div className={uploadClasses.label}>
                  {deadlinePassed || deadlineRejected
                    ? t('game.content.writeup.deadline_exceeded')
                    : progress !== 0
                      ? t('game.button.writeup.uploading')
                      : t('game.button.writeup.upload')}
                </div>
                {progress !== 0 && (
                  <Progress
                    value={progress}
                    className={uploadClasses.progress}
                    color={alpha(theme.colors[theme.primaryColor][2], 0.35)}
                    radius="sm"
                  />
                )}
              </Button>
            )}
          </FileButton>
        </Stack>
      )}
    </Modal>
  )
}

export const isWriteupDeadlineError = (error: unknown): boolean => {
  const response = (
    error as {
      response?: { status?: unknown; data?: { status?: unknown; title?: unknown } }
    }
  )?.response
  const status = response?.data?.status ?? response?.status
  const title = response?.data?.title

  return (
    (status === 400 && title === 'Writeup deadline has passed') ||
    (status === 409 && title === 'Writeup submission is no longer eligible')
  )
}
