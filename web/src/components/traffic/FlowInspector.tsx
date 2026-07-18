import {
  Badge,
  Box,
  Center,
  Grid,
  Group,
  Input,
  Loader,
  Modal,
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
import { FC, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { HunamizeSize } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import { useUrlState } from '@Hooks/useUrlState'
import api, { FlowFilter, TrafficFlowDirection, TrafficFlowSummary } from '@Api'
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
  const [selected, setSelected] = useUrlState<number | null>(
    'port',
    (raw) => {
      if (!raw) return null
      const n = Number.parseInt(raw, 10)
      return Number.isFinite(n) ? n : null
    },
    (v) => (v == null ? null : String(v))
  )

  const [debouncedRegex] = useDebouncedValue(regex, 300)
  const [debouncedPeerIp] = useDebouncedValue(peerIp, 300)

  const [flows, setFlows] = useState<TrafficFlowSummary[]>([])
  const [loading, setLoading] = useState(false)

  useEffect(() => {
    if (!opened) return
    let cancelled = false
    setLoading(true)

    const filter: FlowFilter = {
      ...(debouncedRegex ? { regexPattern: debouncedRegex } : {}),
      ...(debouncedPeerIp ? { peerIpContains: debouncedPeerIp } : {}),
      ...(toApiDirection(direction) ? { direction: toApiDirection(direction) } : {}),
      ...(flagsOnly ? { flagsOnly: true } : {}),
    }

    api.game
      .gameGetTrafficFlows(challengeId!, participationId!, filename!, filter)
      .then((res) => {
        if (!cancelled) setFlows(res.data)
      })
      .catch(() => {
        if (!cancelled) setFlows([])
      })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [opened, challengeId, participationId, filename, debouncedRegex, debouncedPeerIp, direction, flagsOnly])

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
            onChange={(e) => setRegex(e.currentTarget.value)}
            style={{ flex: isCompact ? '1 1 100%' : 1, minWidth: 0 }}
          />
          <TextInput
            size="xs"
            label={t('game.label.flow.filter.peer_ip_label', 'Peer IP')}
            placeholder={t('game.label.flow.filter.peer_ip')}
            value={peerIp}
            onChange={(e) => setPeerIp(e.currentTarget.value)}
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
              onChange={(v) => setDirection(v as DirectionFilter)}
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
            onChange={(e) => setFlagsOnly(e.currentTarget.checked)}
          />
        </Group>

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
            <ScrollArea h="100%" type="auto">
              {loading ? (
                <Center py="xl">
                  <Loader size="sm" />
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
                      const isSelected = selected === flow.connectionPort
                      return (
                        <Table.Tr
                          key={flow.connectionPort}
                          onClick={() => setSelected(flow.connectionPort)}
                          style={{
                            cursor: 'pointer',
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
                              onClick={(event) => {
                                event.stopPropagation()
                                setSelected(flow.connectionPort)
                              }}
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
          </Grid.Col>
          <Grid.Col span={{ base: 12, md: 7 }} h={isCompact ? 'clamp(20rem, 52vh, 32rem)' : '100%'}>
            <Box pl={isCompact ? 0 : 'sm'} h="100%" style={{ overflowY: isCompact ? 'auto' : undefined }}>
              {opened && (
                <FlowDetail
                  challengeId={challengeId!}
                  participationId={participationId!}
                  filename={filename!}
                  connectionPort={selected}
                />
              )}
            </Box>
          </Grid.Col>
        </Grid>
      </Stack>
    </Modal>
  )
}
