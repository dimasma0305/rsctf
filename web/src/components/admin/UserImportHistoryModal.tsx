import {
  ActionIcon,
  Alert,
  Badge,
  Box,
  Button,
  Card,
  Group,
  Loader,
  Modal,
  Paper,
  ScrollArea,
  Stack,
  Table,
  Text,
  TextInput,
  Tooltip,
} from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import {
  mdiAlertCircleOutline,
  mdiArrowLeft,
  mdiArrowLeftBold,
  mdiArrowRightBold,
  mdiCheck,
  mdiEmailArrowRightOutline,
  mdiHistory,
  mdiKeyChange,
  mdiMagnify,
  mdiPencilOutline,
  mdiRefresh,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { isRetryableHttpError } from '@Utils/HttpError'
import { showErrorMsg } from '@Utils/Shared'
import api, { AdminUserImportHistoryDetail, AdminUserImportHistoryRow, AdminUserImportHistorySummary } from '@Api'

const PAGE_SIZE = 20

interface UserImportHistoryModalProps {
  opened: boolean
  onClose: () => void
  onEditUser: (userId: string) => void | Promise<void>
}

const dateTime = (value?: number | null) => (value ? new Date(value).toLocaleString() : '—')

const emailColor = (status: AdminUserImportHistoryRow['emailStatus']) => {
  if (status === 'Sent') return 'teal'
  if (status === 'Failed') return 'red'
  if (status === 'Queued') return 'blue'
  return 'gray'
}

export const UserImportHistoryModal: FC<UserImportHistoryModalProps> = ({ opened, onClose, onEditUser }) => {
  const { t } = useTranslation()
  const [page, setPage] = useState(1)
  const [items, setItems] = useState<AdminUserImportHistorySummary[]>([])
  const [total, setTotal] = useState(0)
  const [detail, setDetail] = useState<AdminUserImportHistoryDetail | null>(null)
  const [loading, setLoading] = useState(false)
  const [rowAction, setRowAction] = useState<number | null>(null)
  const [filter, setFilter] = useState('')
  const passwordEmailOperations = useRef(new Map<string, string>())

  const loadHistory = useCallback(async () => {
    setLoading(true)
    try {
      const response = await api.admin.adminUserImportHistory({
        count: PAGE_SIZE,
        skip: (page - 1) * PAGE_SIZE,
      })
      setItems(response.data.data)
      setTotal(response.data.total ?? response.data.data.length)
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      setLoading(false)
    }
  }, [page, t])

  const loadDetail = useCallback(
    async (operationId: string) => {
      setLoading(true)
      try {
        const response = await api.admin.adminUserImportHistoryDetail(operationId)
        setDetail(response.data)
        setFilter('')
      } catch (error) {
        showErrorMsg(error, t)
      } finally {
        setLoading(false)
      }
    },
    [t]
  )

  useEffect(() => {
    if (opened && !detail) void loadHistory()
  }, [detail, loadHistory, opened])

  useEffect(() => {
    if (!opened) {
      setDetail(null)
      setFilter('')
      setRowAction(null)
    }
  }, [opened])

  const filteredRows = useMemo(() => {
    const query = filter.trim().toLowerCase()
    if (!query) return detail?.rows ?? []
    return (detail?.rows ?? []).filter((row) =>
      [row.email, row.realName, row.userName, row.teamName ?? '', row.status, row.emailStatus]
        .join(' ')
        .toLowerCase()
        .includes(query)
    )
  }, [detail, filter])

  const retryCredentials = async (row: AdminUserImportHistoryRow) => {
    if (!detail) return
    setRowAction(row.rowIndex)
    try {
      const response = await fetch('/api/admin/users/credentials/send', {
        method: 'POST',
        credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          items: [
            {
              email: row.email,
              userName: row.userName,
              importOperationId: detail.operationId,
              importRowIndex: row.rowIndex,
            },
          ],
        }),
      })
      const result = await response.json().catch(() => null)
      if (!response.ok) throw new Error(result?.title ?? 'Credential email failed')
      if (!result?.results?.[0]?.sent) {
        throw new Error(result?.results?.[0]?.error ?? 'Credential email failed')
      }
      showNotification({
        color: 'teal',
        icon: <Icon path={mdiCheck} size={1} />,
        message: `Credentials sent to ${row.email}`,
      })
      await loadDetail(detail.operationId)
    } catch (error) {
      showErrorMsg(error, t)
      await loadDetail(detail.operationId)
    } finally {
      setRowAction(null)
    }
  }

  const sendPasswordLink = async (row: AdminUserImportHistoryRow) => {
    if (!detail || !row.userId) return
    const operationKey = `${detail.operationId}:${row.rowIndex}`
    const operationId = passwordEmailOperations.current.get(operationKey) ?? crypto.randomUUID()
    passwordEmailOperations.current.set(operationKey, operationId)
    setRowAction(row.rowIndex)
    try {
      await api.admin.adminSendPasswordSetupEmail(
        row.userId,
        {
          operationId,
          importOperationId: detail.operationId,
          importRowIndex: row.rowIndex,
        },
        { headers: { 'Idempotency-Key': operationId } }
      )
      showNotification({
        color: 'teal',
        icon: <Icon path={mdiCheck} size={1} />,
        message: `Password setup email queued for ${row.email}`,
      })
      passwordEmailOperations.current.delete(operationKey)
      await loadDetail(detail.operationId)
    } catch (error) {
      if (!isRetryableHttpError(error)) passwordEmailOperations.current.delete(operationKey)
      showErrorMsg(error, t)
    } finally {
      setRowAction(null)
    }
  }

  const rowActions = (row: AdminUserImportHistoryRow) => {
    if (row.status === 'skipped') return null
    const busy = rowAction === row.rowIndex
    return (
      <Group gap="xs" wrap="nowrap" justify="flex-end" aria-label={`Actions for ${row.userName}`}>
        <Tooltip label="Edit current user">
          <ActionIcon
            size={40}
            variant="light"
            color="blue"
            disabled={busy || !row.userExists || !row.userId}
            aria-label={`Edit ${row.userName}`}
            onClick={() => row.userId && onEditUser(row.userId)}
          >
            <Icon path={mdiPencilOutline} size={0.9} />
          </ActionIcon>
        </Tooltip>
        {detail?.credentialsAvailable && row.emailStatus !== 'Sent' && (
          <Tooltip label="Retry the original temporary credentials">
            <ActionIcon
              size={40}
              variant="light"
              color="teal"
              loading={busy}
              disabled={busy}
              aria-label={`Retry original credentials for ${row.email}`}
              onClick={() => void retryCredentials(row)}
            >
              <Icon path={mdiEmailArrowRightOutline} size={0.9} />
            </ActionIcon>
          </Tooltip>
        )}
        <Tooltip label="Send a fresh, single-use password setup link">
          <ActionIcon
            size={40}
            variant="light"
            color="orange"
            loading={busy}
            disabled={busy || !row.userExists || !row.userId}
            aria-label={`Send password setup link to ${row.email}`}
            onClick={() => void sendPasswordLink(row)}
          >
            <Icon path={mdiKeyChange} size={0.9} />
          </ActionIcon>
        </Tooltip>
      </Group>
    )
  }

  return (
    <Modal
      opened={opened}
      onClose={onClose}
      size="min(72rem, calc(100vw - 1rem))"
      title={
        <Group gap="xs">
          <Icon path={mdiHistory} size={1} />
          <Text fw={700}>User import history</Text>
        </Group>
      }
      scrollAreaComponent={ScrollArea.Autosize}
    >
      <Stack gap="md">
        <Group justify="space-between" align="center" wrap="wrap">
          {detail ? (
            <Button
              variant="subtle"
              leftSection={<Icon path={mdiArrowLeft} size={0.8} />}
              onClick={() => setDetail(null)}
            >
              All imports
            </Button>
          ) : (
            <Text size="sm" c="dimmed">
              Non-secret import records are retained for 180 days.
            </Text>
          )}
          <ActionIcon
            size={40}
            variant="light"
            aria-label="Refresh import history"
            disabled={loading}
            onClick={() => void (detail ? loadDetail(detail.operationId) : loadHistory())}
          >
            <Icon path={mdiRefresh} size={0.9} />
          </ActionIcon>
        </Group>

        {loading && <Loader size="sm" aria-label="Loading import history" />}

        {!detail && !loading && items.length === 0 && (
          <Alert icon={<Icon path={mdiHistory} size={1} />} color="blue">
            No retained imports yet. New CSV imports will appear here.
          </Alert>
        )}

        {!detail && items.length > 0 && (
          <Stack gap="sm">
            {items.map((item) => (
              <Card key={item.operationId} withBorder radius="md" p="md">
                <Group justify="space-between" align="flex-start" wrap="wrap">
                  <Stack gap={4}>
                    <Group gap="xs" wrap="wrap">
                      <Text fw={700}>{item.sourceName || 'Imported users'}</Text>
                      <Badge
                        variant="light"
                        color={item.status === 'Running' ? 'blue' : item.status === 'Expired' ? 'gray' : 'teal'}
                      >
                        {item.status}
                      </Badge>
                    </Group>
                    <Text size="xs" c="dimmed">
                      {dateTime(item.createdAtUtc)} by {item.requestedBy}
                    </Text>
                    <Group gap="xs" wrap="wrap">
                      <Badge color="teal" variant="outline">
                        {item.created} created
                      </Badge>
                      <Badge color="blue" variant="outline">
                        {item.updated} updated
                      </Badge>
                      <Badge color="orange" variant="outline">
                        {item.skipped} skipped
                      </Badge>
                      <Badge color="gray" variant="outline">
                        {item.total} total
                      </Badge>
                    </Group>
                  </Stack>
                  <Button
                    variant="light"
                    disabled={!item.detailsAvailable}
                    onClick={() => void loadDetail(item.operationId)}
                  >
                    {item.detailsAvailable ? 'Review import' : 'Details expired'}
                  </Button>
                </Group>
              </Card>
            ))}
            <Group justify="center" role="group" aria-label="Import history pages">
              <ActionIcon
                size={40}
                disabled={page <= 1}
                aria-label="Previous history page"
                onClick={() => setPage((value) => value - 1)}
              >
                <Icon path={mdiArrowLeftBold} size={0.8} />
              </ActionIcon>
              <Text size="sm" aria-live="polite">
                Page {page}
              </Text>
              <ActionIcon
                size={40}
                disabled={page * PAGE_SIZE >= total}
                aria-label="Next history page"
                onClick={() => setPage((value) => value + 1)}
              >
                <Icon path={mdiArrowRightBold} size={0.8} />
              </ActionIcon>
            </Group>
          </Stack>
        )}

        {detail && (
          <Stack gap="md">
            <Paper withBorder p="md">
              <Group justify="space-between" wrap="wrap" align="flex-start">
                <Stack gap={3}>
                  <Text fw={700}>{detail.sourceName || 'Imported users'}</Text>
                  <Text size="xs" c="dimmed">
                    {dateTime(detail.createdAtUtc)} by {detail.requestedBy}
                  </Text>
                  <Text size="sm">
                    {detail.created} created · {detail.updated} updated · {detail.skipped} skipped
                  </Text>
                </Stack>
                <Badge color={detail.credentialsAvailable ? 'teal' : 'gray'} variant="light">
                  {detail.credentialsAvailable ? 'Original credentials available' : 'Original credentials expired'}
                </Badge>
              </Group>
            </Paper>
            <TextInput
              value={filter}
              onChange={(event) => setFilter(event.currentTarget.value)}
              leftSection={<Icon path={mdiMagnify} size={0.8} />}
              label="Filter imported users"
              placeholder="Name, email, team, or status"
            />
            <Text size="xs" c="dimmed" aria-live="polite">
              Showing {filteredRows.length} of {detail.rows.length} rows. A password setup link does not reveal or
              replace the current password until the user opens it.
            </Text>

            <Box visibleFrom="md">
              <ScrollArea type="auto" aria-label="Imported user history rows">
                <Table striped highlightOnHover withTableBorder miw={850}>
                  <Table.Caption>Imported users and email delivery status</Table.Caption>
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th scope="col">User</Table.Th>
                      <Table.Th scope="col">Email</Table.Th>
                      <Table.Th scope="col">Team</Table.Th>
                      <Table.Th scope="col">Import</Table.Th>
                      <Table.Th scope="col">Email</Table.Th>
                      <Table.Th scope="col">
                        <span className="app-sr-only">Actions</span>
                      </Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {filteredRows.map((row) => (
                      <Table.Tr key={row.rowIndex}>
                        <Table.Td>
                          <Text fw={650}>{row.userName}</Text>
                          <Text size="xs" c="dimmed">
                            {row.realName}
                          </Text>
                        </Table.Td>
                        <Table.Td>
                          <Text size="sm" ff="monospace">
                            {row.email || '—'}
                          </Text>
                        </Table.Td>
                        <Table.Td>{row.teamName || '—'}</Table.Td>
                        <Table.Td>
                          <Badge variant="light" color={row.status === 'skipped' ? 'orange' : 'teal'}>
                            {row.status}
                          </Badge>
                          {row.error && (
                            <Text size="xs" c="red">
                              {row.error}
                            </Text>
                          )}
                        </Table.Td>
                        <Table.Td>
                          <Badge variant="light" color={emailColor(row.emailStatus)}>
                            {row.emailStatus}
                          </Badge>
                          {row.emailError && (
                            <Text size="xs" c="red">
                              {row.emailError}
                            </Text>
                          )}
                        </Table.Td>
                        <Table.Td>{rowActions(row)}</Table.Td>
                      </Table.Tr>
                    ))}
                  </Table.Tbody>
                </Table>
              </ScrollArea>
            </Box>

            <Stack hiddenFrom="md" gap="sm">
              {filteredRows.map((row) => (
                <Card key={row.rowIndex} withBorder p="md">
                  <Stack gap="xs">
                    <Group justify="space-between" align="flex-start" wrap="nowrap">
                      <Stack gap={1} style={{ minWidth: 0 }}>
                        <Text fw={700}>{row.userName}</Text>
                        <Text size="sm" style={{ overflowWrap: 'anywhere' }}>
                          {row.email || '—'}
                        </Text>
                        <Text size="xs" c="dimmed">
                          {row.realName}
                          {row.teamName ? ` · ${row.teamName}` : ''}
                        </Text>
                      </Stack>
                      <Badge variant="light" color={row.status === 'skipped' ? 'orange' : 'teal'}>
                        {row.status}
                      </Badge>
                    </Group>
                    {row.error && (
                      <Text size="xs" c="red">
                        {row.error}
                      </Text>
                    )}
                    <Group justify="space-between">
                      <Badge variant="light" color={emailColor(row.emailStatus)}>
                        {row.emailStatus}
                      </Badge>
                      {rowActions(row)}
                    </Group>
                    {row.emailError && (
                      <Text size="xs" c="red">
                        {row.emailError}
                      </Text>
                    )}
                  </Stack>
                </Card>
              ))}
            </Stack>

            {filteredRows.length === 0 && (
              <Alert color="gray" icon={<Icon path={mdiAlertCircleOutline} size={1} />}>
                No rows match this filter.
              </Alert>
            )}
          </Stack>
        )}
      </Stack>
    </Modal>
  )
}
