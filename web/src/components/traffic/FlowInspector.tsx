import {
  Alert,
  Badge,
  Box,
  Button,
  Center,
  Grid,
  Group,
  Input,
  Loader,
  Modal,
  Pagination,
  ScrollArea,
  SegmentedControl,
  Stack,
  Switch,
  Table,
  Text,
  TextInput,
  Title,
  UnstyledButton,
  VisuallyHidden,
} from '@mantine/core'
import { useDebouncedValue } from '@mantine/hooks'
import dayjs from 'dayjs'
import { FC, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { HunamizeSize } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import { useTrafficFlowPage } from '@Hooks/useTrafficInspector'
import { useUrlState } from '@Hooks/useUrlState'
import { TrafficFlowDirection, TrafficFlowQuery } from '@Api'
import { FlowDetail } from './FlowDetail'

interface FlowInspectorProps {
  challengeId: number | null
  participationId: number | null
  filename: string | null
  onClose: () => void
}

type DirectionFilter = 'both' | 'in' | 'out'

const toApiDirection = (d: DirectionFilter): TrafficFlowDirection | undefined =>
  d === 'in' ? 'ContainerToTeam' : d === 'out' ? 'TeamToContainer' : undefined

export const FlowInspector: FC<FlowInspectorProps> = ({ challengeId, participationId, filename, onClose }) => {
  const { t } = useTranslation()
  const isCompact = useIsMobile(992)

  const opened = challengeId != null && participationId != null && filename != null

  const [regex, setRegex] = useUrlState<string>(
    'regex',
    (raw) => raw ?? '',
    (v) => (v.length > 0 ? v : null)
  )
  const [peerIp, setPeerIp] = useUrlState<string>(
    'ip',
    (raw) => raw ?? '',
    (v) => (v.length > 0 ? v : null)
  )
  const [direction, setDirection] = useUrlState<DirectionFilter>(
    'dir',
    (raw) => (raw === 'in' || raw === 'out' ? raw : 'both'),
    (v) => (v === 'both' ? null : v)
  )
  const [flagsOnly, setFlagsOnly] = useUrlState<boolean>(
    'flags',
    (raw) => raw === '1',
    (v) => (v ? '1' : null)
  )
  const [selectedFlowId, setSelectedFlowId] = useUrlState<string | null>(
    'flowId',
    (raw) => {
      if (!raw || raw.length > 76 || raw.length % 2 !== 0 || !/^[a-f\d]+$/i.test(raw)) return null
      return raw.toLowerCase()
    },
    (v) => v
  )
  const setSelectedRef = useRef(setSelectedFlowId)
  setSelectedRef.current = setSelectedFlowId

  const [debouncedRegex] = useDebouncedValue(regex, 300)
  const [debouncedPeerIp] = useDebouncedValue(peerIp, 300)
  const [page, setPage] = useState(1)
  const query = useMemo<TrafficFlowQuery>(
    () => ({
      ...(debouncedRegex ? { regexPattern: debouncedRegex } : {}),
      ...(debouncedPeerIp ? { peerIpContains: debouncedPeerIp } : {}),
      ...(toApiDirection(direction) ? { direction: toApiDirection(direction) } : {}),
      ...(flagsOnly ? { flagsOnly: true } : {}),
      page,
      pageSize: 50,
    }),
    [debouncedRegex, debouncedPeerIp, direction, flagsOnly, page]
  )
  const {
    page: flowPage,
    loading,
    error,
    retryAfterMs,
    retry,
  } = useTrafficFlowPage({
    opened,
    challengeId,
    participationId,
    filename,
    query,
  })
  const flows = flowPage?.items ?? []
  const selectedFlow = flows.find((flow) => flow.flowId === selectedFlowId) ?? null

  useEffect(() => {
    setPage(1)
    setSelectedRef.current(null)
  }, [challengeId, participationId, filename])

  useEffect(() => {
    if (selectedFlowId !== null && flowPage && !flowPage.items.some((flow) => flow.flowId === selectedFlowId)) {
      setSelectedRef.current(null)
    }
  }, [flowPage, selectedFlowId])

  const resetPage = () => setPage(1)

  return (
    <Modal
      opened={opened}
      onClose={onClose}
      fullScreen
      withCloseButton
      title={
        <Group gap="sm" wrap="wrap">
          <Title order={4}>{t('game.label.flow.title')}</Title>
          {filename && (
            <Text size="sm" c="dimmed" ff="monospace" style={{ overflowWrap: 'anywhere' }}>
              {filename}
            </Text>
          )}
        </Group>
      }
      styles={{
        body: {
          height: 'calc(100dvh - 60px)',
          overflowY: isCompact ? 'auto' : 'hidden',
          padding: isCompact ? 'var(--mantine-spacing-xs)' : 'var(--mantine-spacing-md)',
        },
      }}
    >
      <Stack gap="sm" h={isCompact ? 'auto' : '100%'}>
        <Group gap="sm" wrap={isCompact ? 'wrap' : 'nowrap'} align="flex-end">
          <TextInput
            size="xs"
            label={t('game.label.flow.filter.regex_label', 'Payload regex')}
            placeholder={t('game.label.flow.filter.regex')}
            value={regex}
            maxLength={256}
            onChange={(e) => {
              setRegex(e.currentTarget.value)
              resetPage()
            }}
            style={{ flex: isCompact ? '1 1 100%' : 1, minWidth: 0 }}
          />
          <TextInput
            size="xs"
            label={t('game.label.flow.filter.peer_ip_label', 'Peer IP')}
            placeholder={t('game.label.flow.filter.peer_ip')}
            value={peerIp}
            maxLength={64}
            onChange={(e) => {
              setPeerIp(e.currentTarget.value)
              resetPage()
            }}
            w={isCompact ? '100%' : 180}
          />
          <Input.Wrapper
            label={t('game.label.flow.filter.direction.label', 'Direction')}
            w={isCompact ? '100%' : undefined}
          >
            <SegmentedControl
              size="xs"
              fullWidth={isCompact}
              aria-label={t('game.label.flow.filter.direction.label', 'Direction')}
              value={direction}
              onChange={(v) => {
                setDirection(v as DirectionFilter)
                resetPage()
              }}
              data={[
                { value: 'both', label: t('game.label.flow.filter.direction.both') },
                { value: 'in', label: t('game.label.flow.filter.direction.in') },
                { value: 'out', label: t('game.label.flow.filter.direction.out') },
              ]}
            />
          </Input.Wrapper>
          <Switch
            size="xs"
            label={t('game.label.flow.filter.flags_only')}
            checked={flagsOnly}
            onChange={(e) => {
              setFlagsOnly(e.currentTarget.checked)
              resetPage()
            }}
          />
        </Group>

        {error && (
          <Alert
            color="orange"
            role="status"
            aria-live="polite"
            title={t('game.label.flow.refresh_failed', 'Refresh failed')}
          >
            <Group justify="space-between" align="center" wrap="wrap">
              <Text size="sm">
                {error}
                {flowPage && ` ${t('game.label.flow.showing_last_good', 'Showing the last successful result.')}`}
                {retryAfterMs !== null &&
                  ` ${t('game.label.flow.retrying', {
                    defaultValue: 'Retrying in {{seconds}} seconds.',
                    seconds: Math.max(1, Math.ceil(retryAfterMs / 1000)),
                  })}`}
              </Text>
              {retryAfterMs === null && (
                <Button size="xs" variant="light" onClick={retry}>
                  {t('common.retry', 'Retry')}
                </Button>
              )}
            </Group>
          </Alert>
        )}

        {flowPage?.payloadTruncated && (
          <Alert color="yellow" role="status">
            {t(
              'game.label.flow.payload_truncated',
              'Payload detail and regex coverage are bounded; some large-flow payload was not indexed.'
            )}
          </Alert>
        )}

        <Grid gap={isCompact ? 'md' : 0} style={{ flex: isCompact ? undefined : 1, minHeight: 0 }}>
          <Grid.Col
            span={{ base: 12, md: 5 }}
            h={isCompact ? 'clamp(14rem, 36vh, 22rem)' : '100%'}
            style={{
              borderRight: isCompact ? undefined : '1px solid var(--mantine-color-default-border)',
              borderBottom: isCompact ? '1px solid var(--mantine-color-default-border)' : undefined,
              paddingBottom: isCompact ? 'var(--mantine-spacing-sm)' : undefined,
            }}
          >
            <Stack gap={4} h="100%">
              {loading && flowPage && (
                <Group gap="xs" role="status" aria-live="polite" px="xs">
                  <Loader size="xs" />
                  <Text size="xs" c="dimmed">
                    {t('game.label.flow.refreshing', 'Refreshing flows…')}
                  </Text>
                </Group>
              )}
              <ScrollArea
                type="auto"
                style={{ flex: 1, minHeight: 0 }}
                viewportProps={{
                  tabIndex: 0,
                  'aria-label': t('game.label.flow.table_region', 'Captured traffic flow results'),
                }}
              >
                {loading && !flowPage ? (
                  <Center py="xl">
                    <Stack gap="xs" align="center" role="status" aria-live="polite">
                      <Loader size="sm" />
                      <Text size="sm" c="dimmed">
                        {t('game.label.flow.loading', 'Indexing capture…')}
                      </Text>
                    </Stack>
                  </Center>
                ) : flows.length === 0 ? (
                  <Center py="xl">
                    <Text c="dimmed" size="sm">
                      {t('game.label.flow.empty')}
                    </Text>
                  </Center>
                ) : (
                  <Table highlightOnHover striped withTableBorder={false} stickyHeader>
                    <Table.Caption>
                      <VisuallyHidden>{t('game.label.flow.table_caption', 'Captured traffic flows')}</VisuallyHidden>
                    </Table.Caption>
                    <Table.Thead>
                      <Table.Tr>
                        <Table.Th scope="col">{t('game.label.flow.column.time')}</Table.Th>
                        <Table.Th scope="col">{t('game.label.flow.column.peer')}</Table.Th>
                        <Table.Th scope="col">{t('game.label.flow.column.duration')}</Table.Th>
                        <Table.Th scope="col" aria-label={t('game.label.flow.column.bytes_out', 'Bytes sent')}>
                          ↑
                        </Table.Th>
                        <Table.Th scope="col" aria-label={t('game.label.flow.column.bytes_in', 'Bytes received')}>
                          ↓
                        </Table.Th>
                        <Table.Th scope="col" aria-label={t('game.label.flow.column.flag_hits')}>
                          🚩
                        </Table.Th>
                      </Table.Tr>
                    </Table.Thead>
                    <Table.Tbody>
                      {flows.map((flow) => {
                        const dur = dayjs(flow.lastSeenUtc).diff(dayjs(flow.firstSeenUtc), 'millisecond')
                        const isSelected = selectedFlowId === flow.flowId
                        return (
                          <Table.Tr
                            key={flow.flowId}
                            style={{
                              backgroundColor: isSelected ? 'var(--mantine-color-blue-light)' : undefined,
                            }}
                          >
                            <Table.Td ff="monospace" fz="xs">
                              {dayjs(flow.firstSeenUtc).format('HH:mm:ss.SSS')}
                            </Table.Td>
                            <Table.Td ff="monospace" fz="xs">
                              <UnstyledButton
                                aria-label={t('game.label.flow.select', {
                                  defaultValue: 'Inspect flow from {{peer}}',
                                  peer: flow.peerIp,
                                })}
                                aria-pressed={isSelected}
                                onClick={() => setSelectedFlowId(flow.flowId)}
                                style={{ textDecoration: isSelected ? 'underline' : undefined }}
                              >
                                {flow.peerIp}
                              </UnstyledButton>
                            </Table.Td>
                            <Table.Td fz="xs">{dur}ms</Table.Td>
                            <Table.Td fz="xs">{HunamizeSize(flow.bytesOut)}</Table.Td>
                            <Table.Td fz="xs">{HunamizeSize(flow.bytesIn)}</Table.Td>
                            <Table.Td>
                              {flow.flagHits > 0 && (
                                <Badge size="xs" color="yellow" variant="filled">
                                  {flow.flagHits}
                                </Badge>
                              )}
                            </Table.Td>
                          </Table.Tr>
                        )
                      })}
                    </Table.Tbody>
                  </Table>
                )}
              </ScrollArea>
              {flowPage && flowPage.totalPages > 1 && (
                <Pagination.Root
                  value={flowPage.page}
                  onChange={(nextPage) => {
                    setPage(nextPage)
                    setSelectedFlowId(null)
                  }}
                  total={flowPage.totalPages}
                  size="xs"
                  siblings={isCompact ? 0 : 1}
                  aria-label={t('game.label.flow.pagination', 'Flow result pages')}
                >
                  <Group justify="center" gap={4} wrap="nowrap">
                    <Pagination.Previous aria-label={t('common.pagination.previous', 'Previous page')} />
                    <Pagination.Items />
                    <Pagination.Next aria-label={t('common.pagination.next', 'Next page')} />
                  </Group>
                </Pagination.Root>
              )}
              <VisuallyHidden aria-live="polite" aria-atomic="true">
                {flowPage &&
                  t('game.label.flow.result_count', {
                    defaultValue: '{{count}} matching flows, page {{page}} of {{pages}}',
                    count: flowPage.totalItems,
                    page: flowPage.page,
                    pages: Math.max(1, flowPage.totalPages),
                  })}
              </VisuallyHidden>
            </Stack>
          </Grid.Col>
          <Grid.Col span={{ base: 12, md: 7 }} h={isCompact ? 'clamp(20rem, 52vh, 32rem)' : '100%'}>
            <Box pl={isCompact ? 0 : 'sm'} h="100%" style={{ overflowY: isCompact ? 'auto' : undefined }}>
              {opened && (
                <FlowDetail
                  challengeId={challengeId!}
                  participationId={participationId!}
                  filename={filename!}
                  connectionPort={selectedFlow?.connectionPort ?? null}
                  flowId={selectedFlow?.flowId ?? null}
                  snapshotVersion={flowPage?.snapshotVersion ?? null}
                />
              )}
            </Box>
          </Grid.Col>
        </Grid>
      </Stack>
    </Modal>
  )
}
