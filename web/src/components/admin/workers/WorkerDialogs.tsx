import {
  Alert,
  Badge,
  Button,
  Code,
  CopyButton,
  Group,
  Modal,
  Paper,
  Stack,
  Tabs,
  Text,
  TextInput,
  Title,
} from '@mantine/core'
import {
  mdiAlertCircleOutline,
  mdiCheck,
  mdiContentCopy,
  mdiInformationOutline,
  mdiPlus,
  mdiTrashCanOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { CopyCommand } from '@Components/admin/workers/CopyCommand'
import { Enrollment, Worker, WorkerInstallCommands } from '@Components/admin/workers/types'
import classes from '@Styles/AdminWorkers.module.css'

interface WorkerDialogsProps {
  busy: boolean
  commands: WorkerInstallCommands
  createOpened: boolean
  deleteConfirmation: string
  deleteTarget: Worker | null
  enrollment: Enrollment | null
  name: string
  onCloseCreate: () => void
  onCloseDelete: () => void
  onCloseEnrollment: () => void
  onCreate: () => void
  onDelete: () => void
  onDeleteConfirmationChange: (value: string) => void
  onNameChange: (value: string) => void
}

export const WorkerDialogs: FC<WorkerDialogsProps> = ({
  busy,
  commands,
  createOpened,
  deleteConfirmation,
  deleteTarget,
  enrollment,
  name,
  onCloseCreate,
  onCloseDelete,
  onCloseEnrollment,
  onCreate,
  onDelete,
  onDeleteConfirmationChange,
  onNameChange,
}) => {
  const { t } = useTranslation()

  return (
    <>
      <Modal
        opened={createOpened}
        onClose={onCloseCreate}
        title={t('admin.workers.create.title', 'Add a trusted worker')}
        closeOnClickOutside={!busy}
      >
        <Stack gap="md">
          <Text size="sm" c="dimmed">
            {t(
              'admin.workers.create.description',
              'Create the worker record first. RSCTF will then show a short-lived token and verified install commands.'
            )}
          </Text>
          <TextInput
            label={t('admin.workers.create.name', 'Worker name')}
            description={t('admin.workers.create.name_description', 'Use a location or role that operators recognize.')}
            placeholder={t('admin.workers.create.placeholder', 'event-worker-01')}
            value={name}
            onChange={(event) => onNameChange(event.currentTarget.value)}
            onKeyDown={(event) => event.key === 'Enter' && onCreate()}
            maxLength={128}
            autoComplete="off"
            data-autofocus
          />
          <Alert color="blue" icon={<Icon path={mdiInformationOutline} size={0.9} />}>
            {t(
              'admin.workers.create.security',
              'The bootstrap is public, but only this one-time token can enroll the new worker.'
            )}
          </Alert>
          <Group justify="flex-end" className={classes.modalActions}>
            <Button variant="default" disabled={busy} onClick={onCloseCreate}>
              {t('common.button.cancel', 'Cancel')}
            </Button>
            <Button
              leftSection={<Icon path={mdiPlus} size={0.8} />}
              loading={busy}
              disabled={!name.trim()}
              onClick={onCreate}
            >
              {t('admin.workers.create.action', 'Create worker')}
            </Button>
          </Group>
        </Stack>
      </Modal>

      <Modal
        opened={enrollment !== null}
        onClose={onCloseEnrollment}
        title={t('admin.workers.enrollment.title', 'Install and enroll this worker')}
        size="xl"
        closeOnClickOutside={false}
      >
        <Stack gap="lg">
          <Alert
            color="orange"
            icon={<Icon path={mdiAlertCircleOutline} size={0.95} />}
            title={t('admin.workers.enrollment.dedicated_title', 'Dedicated challenge host required')}
          >
            {t(
              'admin.workers.enrollment.dedicated_description',
              'Do not enroll a daily-use computer or a machine containing unrelated secrets. The installer verifies the server and release before changing the host.'
            )}
          </Alert>

          <Paper withBorder p="md" className={classes.tokenPanel}>
            <Stack gap="sm">
              <Group justify="space-between" align="flex-start" wrap="wrap">
                <Stack gap={2}>
                  <Text fw={700}>{t('admin.workers.enrollment.token', 'One-time enrollment token')}</Text>
                  <Text size="xs" c="dimmed">
                    {t('admin.workers.enrollment.expires', 'Expires {{time}} and is shown only once.', {
                      time: enrollment ? dayjs(enrollment.expiresAt).fromNow() : '',
                    })}
                  </Text>
                </Stack>
                <Badge color="orange">{t('admin.workers.enrollment.sensitive', 'Sensitive')}</Badge>
              </Group>
              <Code block className={classes.token}>
                {enrollment?.token}
              </Code>
              <CopyButton value={enrollment?.token ?? ''} timeout={1800}>
                {({ copied, copy }) => (
                  <Button
                    color={copied ? 'teal' : 'orange'}
                    variant="light"
                    leftSection={<Icon path={copied ? mdiCheck : mdiContentCopy} size={0.8} />}
                    onClick={copy}
                  >
                    {copied
                      ? t('admin.workers.enrollment.token_copied', 'Token copied')
                      : t('admin.workers.enrollment.copy_token', 'Copy one-time token')}
                  </Button>
                )}
              </CopyButton>
            </Stack>
          </Paper>

          <Stack gap={4}>
            <Title order={3} size="h5">
              {t('admin.workers.enrollment.run_installer', 'Run the installer')}
            </Title>
            <Text size="sm" c="dimmed">
              {t(
                'admin.workers.enrollment.prompt_note',
                'The token is intentionally kept out of the command so it does not remain in shell history. Paste it only when the installer prompts.'
              )}
            </Text>
          </Stack>

          <Tabs defaultValue="linux">
            <Tabs.List grow>
              <Tabs.Tab value="linux">{t('admin.workers.platform.linux', 'Linux')}</Tabs.Tab>
              <Tabs.Tab value="windows">
                {t('admin.workers.platform.windows_admin', 'Windows · Administrator PowerShell')}
              </Tabs.Tab>
            </Tabs.List>
            <Tabs.Panel value="linux" pt="md">
              <CopyCommand
                command={commands.linux}
                label={t('admin.workers.copy_linux_install', 'Copy Linux install command')}
                copiedLabel={t('common.content.copied', 'Copied')}
              />
              <Text size="xs" c="dimmed" mt="sm">
                {t(
                  'admin.workers.enrollment.linux_note',
                  'Uses systemd when available and Docker supervision otherwise. Keep storage quota checks enabled for events.'
                )}
              </Text>
            </Tabs.Panel>
            <Tabs.Panel value="windows" pt="md">
              <CopyCommand
                command={commands.windows}
                label={t('admin.workers.copy_windows_install', 'Copy Windows install command')}
                copiedLabel={t('common.content.copied', 'Copied')}
              />
              <Text size="xs" c="dimmed" mt="sm">
                {t(
                  'admin.workers.enrollment.windows_note',
                  'Native Windows workers require Docker in Windows-container mode. Run PowerShell as Administrator.'
                )}
              </Text>
            </Tabs.Panel>
          </Tabs>
        </Stack>
      </Modal>

      <Modal
        opened={deleteTarget !== null}
        onClose={onCloseDelete}
        title={
          deleteTarget
            ? t('admin.workers.delete.title_named', 'Delete retired worker {{name}}', { name: deleteTarget.name })
            : t('admin.workers.delete.title', 'Delete retired worker')
        }
        centered
        closeOnClickOutside={!busy}
      >
        <Stack gap="md">
          <Alert color="red" icon={<Icon path={mdiAlertCircleOutline} size={0.9} />}>
            {t(
              'admin.workers.delete.warning',
              'This permanently revokes the registered certificate and any outstanding enrollment token. Records with workload history remain available for audit and cannot be deleted.'
            )}
          </Alert>
          <TextInput
            label={
              deleteTarget
                ? t('admin.workers.delete.confirm_label', 'Type {{name}} to confirm', { name: deleteTarget.name })
                : t('admin.workers.create.name', 'Worker name')
            }
            value={deleteConfirmation}
            onChange={(event) => onDeleteConfirmationChange(event.currentTarget.value)}
            data-autofocus
            autoComplete="off"
            onKeyDown={(event) => {
              if (event.key === 'Enter' && deleteTarget && deleteConfirmation === deleteTarget.name) {
                onDelete()
              }
            }}
          />
          <Group justify="flex-end" className={classes.modalActions}>
            <Button variant="default" onClick={onCloseDelete} disabled={busy}>
              {t('common.button.cancel', 'Cancel')}
            </Button>
            <Button
              color="red"
              leftSection={<Icon path={mdiTrashCanOutline} size={0.8} />}
              disabled={!deleteTarget || deleteConfirmation !== deleteTarget.name}
              loading={busy}
              onClick={onDelete}
            >
              {t('admin.workers.delete.action_short', 'Delete worker')}
            </Button>
          </Group>
        </Stack>
      </Modal>
    </>
  )
}
