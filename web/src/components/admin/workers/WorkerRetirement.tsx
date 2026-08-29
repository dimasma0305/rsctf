import { Accordion, Alert, Group, Paper, SimpleGrid, Stack, Tabs, Text, ThemeIcon, Title } from '@mantine/core'
import { mdiAlertCircleOutline, mdiInformationOutline, mdiTrashCanOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { CopyCommand } from '@Components/admin/workers/CopyCommand'
import { WorkerInstallCommands } from '@Components/admin/workers/types'
import classes from '@Styles/AdminWorkers.module.css'

interface WorkerRetirementProps {
  commands: WorkerInstallCommands | null
}

export const WorkerRetirement: FC<WorkerRetirementProps> = ({ commands }) => {
  const { t } = useTranslation()

  const steps = [
    {
      number: '01',
      title: t('admin.workers.retire.disable', 'Disable or drain'),
      description: t('admin.workers.retire.disable_description', 'Stop new placements and clear workloads.'),
    },
    {
      number: '02',
      title: t('admin.workers.retire.uninstall', 'Run uninstall'),
      description: t('admin.workers.retire.uninstall_description', 'Remove the agent and its local identity.'),
    },
    {
      number: '03',
      title: t('admin.workers.retire.delete', 'Delete the record'),
      description: t('admin.workers.retire.delete_description', 'Once offline, revoke it from the inventory.'),
    },
  ]

  return (
    <Paper component="section" withBorder p="lg" className={classes.retirement} aria-labelledby="retire-worker">
      <Group align="flex-start" gap="md" wrap="nowrap">
        <ThemeIcon color="orange" variant="light" size={44} radius="md">
          <Icon path={mdiTrashCanOutline} size={1} aria-hidden="true" />
        </ThemeIcon>
        <Stack gap={3}>
          <Title order={2} size="h4" id="retire-worker">
            {t('admin.workers.retire.title', 'Retire a worker safely')}
          </Title>
          <Text size="sm" c="dimmed">
            {t(
              'admin.workers.retire.description',
              'Drain workloads before removing the local agent, then delete its offline record to revoke the identity.'
            )}
          </Text>
        </Stack>
      </Group>

      <SimpleGrid cols={{ base: 1, sm: 3 }} spacing="sm" mt="lg">
        {steps.map((step) => (
          <div key={step.number} className={classes.step}>
            <Text className={classes.stepNumber}>{step.number}</Text>
            <Text fw={700} size="sm">
              {step.title}
            </Text>
            <Text size="xs" c="dimmed">
              {step.description}
            </Text>
          </div>
        ))}
      </SimpleGrid>

      <Accordion mt="md">
        <Accordion.Item value="uninstall">
          <Accordion.Control icon={<Icon path={mdiInformationOutline} size={0.85} aria-hidden="true" />}>
            {t('admin.workers.retire.commands', 'Show verified uninstall commands')}
          </Accordion.Control>
          <Accordion.Panel>
            <Alert
              color="orange"
              icon={<Icon path={mdiAlertCircleOutline} size={0.9} />}
              mb="md"
              title={t('admin.workers.retire.warning_title', 'Disable the worker first')}
            >
              {t(
                'admin.workers.retire.warning',
                'The installer refuses to remove a host that still has managed workloads and asks before deleting its certificate and configuration.'
              )}
            </Alert>
            {commands ? (
              <Tabs defaultValue="linux">
                <Tabs.List grow>
                  <Tabs.Tab value="linux">{t('admin.workers.platform.linux', 'Linux')}</Tabs.Tab>
                  <Tabs.Tab value="windows">{t('admin.workers.platform.windows', 'Windows PowerShell')}</Tabs.Tab>
                </Tabs.List>
                <Tabs.Panel value="linux" pt="md">
                  <CopyCommand
                    command={commands.linuxUninstall}
                    label={t('admin.workers.copy_linux_uninstall', 'Copy Linux uninstall command')}
                    copiedLabel={t('common.content.copied', 'Copied')}
                  />
                </Tabs.Panel>
                <Tabs.Panel value="windows" pt="md">
                  <CopyCommand
                    command={commands.windowsUninstall}
                    label={t('admin.workers.copy_windows_uninstall', 'Copy Windows uninstall command')}
                    copiedLabel={t('common.content.copied', 'Copied')}
                  />
                </Tabs.Panel>
              </Tabs>
            ) : (
              <Alert color="blue" icon={<Icon path={mdiInformationOutline} size={0.9} />}>
                {t(
                  'admin.workers.install.https_required',
                  'Verified install commands are available when this page is opened from an HTTPS origin.'
                )}
              </Alert>
            )}
          </Accordion.Panel>
        </Accordion.Item>
      </Accordion>
    </Paper>
  )
}
