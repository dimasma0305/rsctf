import { Badge, Center, Flex, Group, Loader, SegmentedControl, Stack, Text } from '@mantine/core'
import dayjs from 'dayjs'
import { FC, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { boundedRetryDelay } from '@Utils/HttpError'
import { HunamizeSize } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import { useUrlState } from '@Hooks/useUrlState'
import api, { TrafficFlowChunk, TrafficFlowDetail, TrafficFlowDirection } from '@Api'
import { HexAsciiView, ViewMode } from './HexAsciiView'

interface FlowDetailProps {
  challengeId: number
  participationId: number
  filename: string
  connectionPort: number | null
  peerIp: string | null
}

const decodeBase64 = (s: string): Uint8Array => {
  const binary = atob(s)
  const bytes = new Uint8Array(binary.length)
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i)
  return bytes
}

const concatChunks = (
  chunks: TrafficFlowChunk[],
  direction: TrafficFlowDirection
): { bytes: Uint8Array; flagOffsets: number[] } => {
  const buffers: Uint8Array[] = []
  const flagOffsets: number[] = []
  let cursor = 0
  for (const c of chunks) {
    if (c.direction !== direction) continue
    const bytes = decodeBase64(c.payloadBase64)
    buffers.push(bytes)
    for (const off of c.flagOffsets) flagOffsets.push(cursor + off)
    cursor += bytes.length
  }
  let total = 0
  for (const b of buffers) total += b.length
  const out = new Uint8Array(total)
  let pos = 0
  for (const b of buffers) {
    out.set(b, pos)
    pos += b.length
  }
  return { bytes: out, flagOffsets }
}

export const FlowDetail: FC<FlowDetailProps> = ({ challengeId, participationId, filename, connectionPort, peerIp }) => {
  const { t } = useTranslation()
  const isCompact = useIsMobile(700)
  const [detail, setDetail] = useState<TrafficFlowDetail | null>(null)
  const [loading, setLoading] = useState(false)
  const [retryGeneration, setRetryGeneration] = useState(0)
  const retryOwner = useRef<{ key: string; attempts: number; timer: number | null }>({
    key: '',
    attempts: 0,
    timer: null,
  })
  const [mode, setMode] = useUrlState<ViewMode>(
    'mode',
    (raw) => (raw === 'hex' ? 'hex' : 'ascii'),
    (v) => (v === 'hex' ? 'hex' : null)
  )

  useEffect(() => {
    if (connectionPort == null || peerIp == null) {
      setDetail(null)
      return
    }
    const requestKey = JSON.stringify([challengeId, participationId, filename, connectionPort, peerIp])
    const owner = retryOwner.current
    if (owner.key !== requestKey) {
      if (owner.timer !== null) window.clearTimeout(owner.timer)
      owner.key = requestKey
      owner.attempts = 0
      owner.timer = null
      setDetail(null)
    }
    const abort = new AbortController()
    setLoading(true)
    api.game
      .gameGetTrafficFlowDetail(
        challengeId,
        participationId,
        filename,
        connectionPort,
        { signal: abort.signal },
        { peerIp }
      )
      .then((res) => {
        if (!abort.signal.aborted) {
          owner.attempts = 0
          setDetail(res.data)
        }
      })
      .catch((error: unknown) => {
        if (!abort.signal.aborted) {
          const delay = boundedRetryDelay(error, owner.attempts)
          if (delay !== null && owner.key === requestKey) {
            owner.attempts += 1
            owner.timer = window.setTimeout(() => {
              owner.timer = null
              setRetryGeneration((generation) => generation + 1)
            }, delay)
          }
        }
      })
      .finally(() => {
        if (!abort.signal.aborted) setLoading(false)
      })
    return () => {
      abort.abort()
      if (owner.timer !== null) {
        window.clearTimeout(owner.timer)
        owner.timer = null
      }
    }
  }, [challengeId, participationId, filename, connectionPort, peerIp, retryGeneration])

  const out = useMemo(() => (detail ? concatChunks(detail.chunks, 'TeamToContainer') : null), [detail])
  const inn = useMemo(() => (detail ? concatChunks(detail.chunks, 'ContainerToTeam') : null), [detail])

  if (connectionPort == null || peerIp == null) {
    return (
      <Center h="100%">
        <Text c="dimmed" size="sm">
          {t('game.label.flow.detail.empty')}
        </Text>
      </Center>
    )
  }

  if (loading) {
    return (
      <Center h="100%">
        <Loader />
      </Center>
    )
  }

  if (!detail) {
    return (
      <Center h="100%">
        <Text c="dimmed" size="sm">
          {t('game.label.flow.detail.empty')}
        </Text>
      </Center>
    )
  }

  return (
    <Stack gap="xs" h="100%">
      <Group justify="space-between" wrap={isCompact ? 'wrap' : 'nowrap'} align="flex-start">
        <Stack gap={0} style={{ minWidth: 0 }}>
          <Text size="sm" fw="bold" style={{ overflowWrap: 'anywhere' }}>
            #{detail.connectionPort} · {detail.peerIp}
          </Text>
          <Text size="xs" c="dimmed">
            {dayjs(detail.firstSeenUtc).format('HH:mm:ss.SSS')} → {dayjs(detail.lastSeenUtc).format('HH:mm:ss.SSS')}
          </Text>
        </Stack>
        <Group gap="xs" wrap="wrap">
          {detail.flagHits > 0 && (
            <Badge color="yellow" variant="filled">
              {t('game.label.flow.column.flag_hits')}: {detail.flagHits}
            </Badge>
          )}
          <SegmentedControl
            size="xs"
            aria-label={t('game.label.flow.detail.view_mode', 'Flow display mode')}
            value={mode}
            onChange={(v) => setMode(v as ViewMode)}
            data={[
              { value: 'ascii', label: t('game.label.flow.detail.ascii') },
              { value: 'hex', label: t('game.label.flow.detail.hex') },
            ]}
          />
        </Group>
      </Group>

      <Flex direction={isCompact ? 'column' : 'row'} align="stretch" gap="xs" style={{ flex: 1, minHeight: 0 }}>
        <Stack gap={4} style={{ minWidth: 0, flex: 1 }}>
          <Group justify="space-between">
            <Text size="xs" fw="bold" c="blue">
              ↑ {t('game.label.flow.filter.direction.out')}
            </Text>
            <Text size="xs" c="dimmed">
              {HunamizeSize(out?.bytes.length ?? 0)}
            </Text>
          </Group>
          {out && (
            <HexAsciiView
              bytes={out.bytes}
              mode={mode}
              flagOffsets={out.flagOffsets}
              style={isCompact ? { maxHeight: 'clamp(10rem, 28vh, 14rem)' } : undefined}
            />
          )}
        </Stack>
        <Stack gap={4} style={{ minWidth: 0, flex: 1 }}>
          <Group justify="space-between">
            <Text size="xs" fw="bold" c="teal">
              ↓ {t('game.label.flow.filter.direction.in')}
            </Text>
            <Text size="xs" c="dimmed">
              {HunamizeSize(inn?.bytes.length ?? 0)}
            </Text>
          </Group>
          {inn && (
            <HexAsciiView
              bytes={inn.bytes}
              mode={mode}
              flagOffsets={inn.flagOffsets}
              style={isCompact ? { maxHeight: 'clamp(10rem, 28vh, 14rem)' } : undefined}
            />
          )}
        </Stack>
      </Flex>
    </Stack>
  )
}
