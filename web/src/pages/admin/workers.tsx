import {
  Badge,
  Button,
  Code,
  CopyButton,
  Group,
  Modal,
  Paper,
  Select,
  Stack,
  Table,
  Text,
  TextInput,
  Title,
} from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiContentCopy, mdiKeyChange, mdiPlus, mdiRefresh } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import relativeTime from 'dayjs/plugin/relativeTime'
import { FC, useCallback, useEffect, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { AdminPage } from '@Components/admin/AdminPage'
import { showErrorMsg } from '@Utils/Shared'
import {
  workerInstallCommand,
  workerUninstallCommand,
  workerWindowsInstallCommand,
  workerWindowsUninstallCommand,
} from '@Utils/WorkerInstall'
import api, { ContentType } from '@Api'

dayjs.extend(relativeTime)

type WorkerState = 'Enabled' | 'Draining' | 'Disabled'

interface WorkerCapacity {
  cpuMillis: number
  memoryBytes: number
  slots: number
}

interface Worker {
  id: string
  name: string
  administrativeState: WorkerState
  platformOs?: string | null
  architecture?: string | null
  runtimeKind?: string | null
  runtimeVersion?: string | null
  capacity: WorkerCapacity
  online: boolean
  heartbeatAt?: number | null
}

interface Enrollment {
  workerId: string
  token: string
  expiresAt: number
}

interface CreatedWorker {
  worker: Worker
  enrollment: Enrollment
}

const Workers: FC = () => {
  const { t } = useTranslation()
  const [workers, setWorkers] = useState<Worker[]>([])
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState(false)
  const [name, setName] = useState('')
  const [enrollment, setEnrollment] = useState<Enrollment | null>(null)

  const loadWorkers = useCallback(async () => {
    try {
      const response = await api.request<Worker[]>({
        path: '/api/admin/workers',
        method: 'GET',
        format: 'json',
      })
      setWorkers(response.data)
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      setLoading(false)
    }
  }, [t])

  useEffect(() => {
    loadWorkers()
    const timer = window.setInterval(loadWorkers, 10_000)
    return () => window.clearInterval(timer)
  }, [loadWorkers])

  const installCommands = useMemo(() => {
    return {
      linux: workerInstallCommand(window.location.origin),
      windows: workerWindowsInstallCommand(window.location.origin),
      linuxUninstall: workerUninstallCommand(window.location.origin),
      windowsUninstall: workerWindowsUninstallCommand(window.location.origin),
    }
  }, [])

  const createWorker = async () => {
    if (!name.trim()) return
    setBusy(true)
    try {
      const response = await api.request<CreatedWorker>({
        path: '/api/admin/workers',
        method: 'POST',
        type: ContentType.Json,
        format: 'json',
        body: { name: name.trim() },
      })
      setName('')
      setEnrollment(response.data.enrollment)
      await loadWorkers()
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      setBusy(false)
    }
  }

  const issueToken = async (worker: Worker) => {
    setBusy(true)
    try {
      const response = await api.request<Enrollment>({
        path: `/api/admin/workers/${worker.id}/token`,
        method: 'POST',
        format: 'json',
      })
      setEnrollment(response.data)
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      setBusy(false)
    }
  }

  const updateState = async (worker: Worker, state: WorkerState) => {
    setBusy(true)
    try {
      await api.request<Worker>({
        path: `/api/admin/workers/${worker.id}/state`,
        method: 'PUT',
        type: ContentType.Json,
        format: 'json',
        body: { state },
      })
      await loadWorkers()
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      setBusy(false)
    }
  }

  const copied = (message: string) =>
    showNotification({ color: 'teal', message, icon: <Icon path={mdiCheck} size={0.8} /> })

  return (
    <AdminPage isLoading={loading}>
      <Stack gap="lg">
        <Group justify="space-between">
          <Title order={2}>Trusted workers</Title>
          <Button variant="light" leftSection={<Icon path={mdiRefresh} size={0.8} />} onClick={loadWorkers}>
            Refresh
          </Button>
        </Group>

        <Paper withBorder radius="md" p="md">
          <Stack gap="sm">
            <Title order={4}>Create worker</Title>
            <Group align="end">
              <TextInput
                label="Worker name"
                placeholder="event-worker-01"
                value={name}
                onChange={(event) => setName(event.currentTarget.value)}
                onKeyDown={(event) => event.key === 'Enter' && createWorker()}
                flex={1}
                maxLength={128}
              />
              <Button
                leftSection={<Icon path={mdiPlus} size={0.8} />}
                loading={busy}
                disabled={!name.trim()}
                onClick={createWorker}
              >
                Create and enroll
              </Button>
            </Group>
            <Text size="sm" c="dimmed">
              The install command is public, but connecting requires the one-time token shown after creation.
            </Text>
          </Stack>
        </Paper>

        <Paper withBorder radius="md" p="md">
          <Table.ScrollContainer minWidth={850}>
            <Table striped highlightOnHover>
              <Table.Thead>
                <Table.Tr>
                  <Table.Th>Worker</Table.Th>
                  <Table.Th>Status</Table.Th>
                  <Table.Th>Platform</Table.Th>
                  <Table.Th>Capacity</Table.Th>
                  <Table.Th>Last heartbeat</Table.Th>
                  <Table.Th>State</Table.Th>
                  <Table.Th>Enrollment</Table.Th>
                </Table.Tr>
              </Table.Thead>
              <Table.Tbody>
                {workers.map((worker) => (
                  <Table.Tr key={worker.id}>
                    <Table.Td>
                      <Text fw={500}>{worker.name}</Text>
                      <Text size="xs" c="dimmed" ff="monospace">
                        {worker.id}
                      </Text>
                    </Table.Td>
                    <Table.Td>
                      <Badge color={worker.online ? 'teal' : 'gray'}>{worker.online ? 'Online' : 'Offline'}</Badge>
                    </Table.Td>
                    <Table.Td>
                      {worker.platformOs
                        ? `${worker.platformOs}/${worker.architecture ?? 'unknown'} · ${worker.runtimeKind ?? 'unknown'}`
                        : 'Not enrolled'}
                    </Table.Td>
                    <Table.Td>
                      {worker.capacity.slots} slots · {worker.capacity.cpuMillis}m CPU
                    </Table.Td>
                    <Table.Td>{worker.heartbeatAt ? dayjs(worker.heartbeatAt).fromNow() : 'Never'}</Table.Td>
                    <Table.Td>
                      <Select
                        aria-label={`Administrative state for ${worker.name}`}
                        data={['Enabled', 'Draining', 'Disabled']}
                        value={worker.administrativeState}
                        disabled={busy}
                        allowDeselect={false}
                        onChange={(value) => value && updateState(worker, value as WorkerState)}
                      />
                    </Table.Td>
                    <Table.Td>
                      <Button
                        size="xs"
                        variant="light"
                        leftSection={<Icon path={mdiKeyChange} size={0.7} />}
                        disabled={busy}
                        onClick={() => issueToken(worker)}
                      >
                        New token
                      </Button>
                    </Table.Td>
                  </Table.Tr>
                ))}
                {workers.length === 0 && (
                  <Table.Tr>
                    <Table.Td colSpan={7}>
                      <Text ta="center" c="dimmed">
                        No trusted workers configured.
                      </Text>
                    </Table.Td>
                  </Table.Tr>
                )}
              </Table.Tbody>
            </Table>
          </Table.ScrollContainer>
        </Paper>

        <Paper withBorder radius="md" p="md">
          <Stack gap="sm">
            <Title order={4}>Uninstall worker software</Title>
            <Text size="sm" c="dimmed">
              First set the worker to Disabled above. Uninstall refuses to remove a host that still has managed
              workloads and asks for confirmation before deleting its local certificate and configuration.
            </Text>
            <Text size="sm" fw={500}>
              Linux
            </Text>
            <Code block>{installCommands.linuxUninstall}</Code>
            <CopyButton value={installCommands.linuxUninstall} timeout={1500}>
              {({ copy }) => (
                <Button variant="light" leftSection={<Icon path={mdiContentCopy} size={0.8} />} onClick={copy}>
                  Copy Linux uninstall command
                </Button>
              )}
            </CopyButton>
            <Text size="sm" fw={500}>
              Windows (Administrator PowerShell)
            </Text>
            <Code block>{installCommands.windowsUninstall}</Code>
            <CopyButton value={installCommands.windowsUninstall} timeout={1500}>
              {({ copy }) => (
                <Button variant="light" leftSection={<Icon path={mdiContentCopy} size={0.8} />} onClick={copy}>
                  Copy Windows uninstall command
                </Button>
              )}
            </CopyButton>
          </Stack>
        </Paper>
      </Stack>

      <Modal
        opened={enrollment !== null}
        onClose={() => setEnrollment(null)}
        title="Install and enroll this worker"
        size="lg"
        closeOnClickOutside={false}
      >
        <Stack gap="md">
          <Text size="sm">
            Run one command on a dedicated Linux or Windows-container host. It verifies the release and privately
            prompts for a dedicated-host acknowledgement and the token below. Do not use a daily-use computer or a
            machine containing unrelated secrets.
          </Text>
          <Text size="sm" fw={500}>
            Linux
          </Text>
          <Code block>{installCommands.linux}</Code>
          <CopyButton value={installCommands.linux} timeout={1500}>
            {({ copy }) => (
              <Button
                variant="light"
                leftSection={<Icon path={mdiContentCopy} size={0.8} />}
                onClick={() => {
                  copy()
                  copied('Install command copied')
                }}
              >
                Copy Linux command
              </Button>
            )}
          </CopyButton>

          <Text size="sm" fw={500}>
            Windows (Administrator PowerShell)
          </Text>
          <Code block>{installCommands.windows}</Code>
          <CopyButton value={installCommands.windows} timeout={1500}>
            {({ copy }) => (
              <Button
                variant="light"
                leftSection={<Icon path={mdiContentCopy} size={0.8} />}
                onClick={() => {
                  copy()
                  copied('Windows install command copied')
                }}
              >
                Copy Windows command
              </Button>
            )}
          </CopyButton>

          <Text size="sm" fw={500}>
            One-time token (expires {enrollment ? dayjs(enrollment.expiresAt).fromNow() : ''})
          </Text>
          <Code block>{enrollment?.token}</Code>
          <CopyButton value={enrollment?.token ?? ''} timeout={1500}>
            {({ copy }) => (
              <Button
                color="orange"
                variant="light"
                leftSection={<Icon path={mdiContentCopy} size={0.8} />}
                onClick={() => {
                  copy()
                  copied('One-time token copied')
                }}
              >
                Copy one-time token
              </Button>
            )}
          </CopyButton>
          <Text size="xs" c="dimmed">
            The token is shown once, expires after 15 minutes, and is consumed by the first successful enrollment.
            Linux automatically uses systemd when available or Docker supervision otherwise. Native Windows workers
            require Docker in Windows-container mode. For Docker Desktop Linux-container mode, enable host networking
            and run the Linux command inside its dedicated Linux VM. Keep storage quota checks enabled for real events.
          </Text>
        </Stack>
      </Modal>
    </AdminPage>
  )
}

export default Workers
