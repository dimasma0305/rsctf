import {
  ActionIcon,
  Alert,
  Badge,
  Button,
  Code,
  CopyButton,
  Group,
  Modal,
  ScrollArea,
  Stack,
  Switch,
  Table,
  Text,
  Title,
  Tooltip,
  UnstyledButton,
} from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import {
  mdiAlertCircle,
  mdiCheck,
  mdiCheckCircle,
  mdiCloseCircle,
  mdiConsole,
  mdiCrown,
  mdiHelpCircle,
  mdiRestart,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { showErrorMsg } from '@Utils/Shared'
import { isKothResetTransition } from '@Utils/kothLifecycle'
import {
  type AdminKothAuditReceipt,
  type AdminKothHill,
  type AdminKothReceiptsModel,
  type AdminKothStateModel,
} from '@Hooks/useGame'
import api from '@Api'
import tableClasses from '@Styles/AdOpsTable.module.css'
import misc from '@Styles/Misc.module.css'

const statusMeta = (status?: string | null): { color: string; icon: string } => {
  switch (status) {
    case 'Ok':
      return { color: 'teal', icon: mdiCheckCircle }
    case 'Mumble':
      return { color: 'yellow', icon: mdiAlertCircle }
    case 'Offline':
      return { color: 'red', icon: mdiCloseCircle }
    case 'InternalError':
      return { color: 'gray', icon: mdiHelpCircle }
    default:
      return { color: 'gray', icon: mdiHelpCircle }
  }
}

const fmtPts = (value: number): string => (Number.isInteger(value) ? String(value) : value.toFixed(1))
const shortId = (value: string) => (value.length > 16 ? `${value.slice(0, 12)}…` : value)
const formatJson = (value: unknown): string => {
  try {
    return JSON.stringify(value, null, 2) ?? String(value)
  } catch {
    return String(value)
  }
}

const CopyId: FC<{ label: string; value?: string | null }> = ({ label, value }) => {
  const { t } = useTranslation()
  if (!value) return null
  return (
    <CopyButton value={value}>
      {({ copied, copy }) => (
        <Tooltip label={copied ? t('game.tooltip.copy.copied', 'Copied') : value} withArrow>
          <UnstyledButton onClick={copy} aria-label={t('game.tooltip.copy.value', 'Copy {{label}}', { label })}>
            <Text size="xs" c="dimmed" className={misc.ffmono}>
              {label}: {shortId(value)}
            </Text>
          </UnstyledButton>
        </Tooltip>
      )}
    </CopyButton>
  )
}

const AuditReceiptDetails: FC<{ entry: AdminKothAuditReceipt }> = ({ entry }) => {
  const { t } = useTranslation()
  const [opened, setOpened] = useState(false)

  return (
    <details onToggle={(event) => setOpened(event.currentTarget.open)}>
      <summary style={{ cursor: 'pointer' }}>
        <Group component="span" gap="xs" wrap="wrap">
          <Badge size="xs" color="gray" variant="light">
            #{entry.id}
          </Badge>
          <Text component="span" size="sm" fw={600}>
            {entry.phase}
          </Text>
          <Text component="span" size="xs" c="dimmed">
            attempt {entry.attempt} · {new Date(entry.createdAt).toLocaleString()}
          </Text>
        </Group>
      </summary>
      {opened && (
        <Stack gap="xs" mt="xs">
          <Text size="xs" fw={700}>
            {t('admin.content.ad_ops.koth.receipt_json', 'Audit receipt')}
          </Text>
          <Code block className={misc.ffmono} style={{ whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }}>
            {formatJson(entry.receipt)}
          </Code>
          <Text size="xs" fw={700}>
            {t('admin.content.ad_ops.koth.filesystem_diff', 'Filesystem diff')}
          </Text>
          {entry.filesystemDiff == null ? (
            <Text size="xs" c="dimmed">
              {t(
                'admin.content.ad_ops.koth.filesystem_diff_unavailable',
                'No filesystem diff was available for this phase.'
              )}
            </Text>
          ) : (
            <Code block className={misc.ffmono} style={{ whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }}>
              {formatJson(entry.filesystemDiff)}
            </Code>
          )}
        </Stack>
      )}
    </details>
  )
}

export interface KothOpsPanelProps {
  gameId: number
  koth: AdminKothStateModel
  onShell: (guid: string, title: string) => void
  onToggleHill: (hill: AdminKothHill) => void
  busyHill: number | null
  onMutate: () => Promise<unknown>
}

export const KothOpsPanel: FC<KothOpsPanelProps> = ({ gameId, koth, onShell, onToggleHill, busyHill, onMutate }) => {
  const { t } = useTranslation()
  const [retryingHill, setRetryingHill] = useState<number | null>(null)
  const [auditHill, setAuditHill] = useState<AdminKothHill | null>(null)
  const [audit, setAudit] = useState<AdminKothReceiptsModel | null>(null)
  const [auditLoading, setAuditLoading] = useState(false)
  const enabledHills = useMemo(() => koth.hills.filter((hill) => hill.isEnabled), [koth.hills])
  const hasResetInProgress = useMemo(
    () => koth.hills.some((hill) => hill.isEnabled && hill.cycleNumber > 0 && isKothResetTransition(hill.resetPhase)),
    [koth.hills]
  )

  const openReceipts = async (hill: AdminKothHill) => {
    setAuditHill(hill)
    setAudit(null)
    setAuditLoading(true)
    try {
      const response = await api.request<AdminKothReceiptsModel | { data: AdminKothReceiptsModel }>({
        path: `/api/edit/games/${gameId}/ad/koth/${hill.challengeId}/receipts`,
        method: 'GET',
        format: 'json',
      })
      const body = response.data
      setAudit('data' in body ? body.data : body)
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      setAuditLoading(false)
    }
  }

  const recoverHill = async (hill: AdminKothHill) => {
    setRetryingHill(hill.challengeId)
    try {
      await api.request({
        path: `/api/edit/games/${gameId}/ad/koth/${hill.challengeId}/recover`,
        method: 'POST',
        format: 'json',
      })
      showNotification({
        color: 'teal',
        icon: <Icon path={mdiCheck} size={1} />,
        message: t(
          'admin.notification.ad_ops.koth.recovery_resumed',
          'The idempotent hill recovery path resumed successfully.'
        ),
      })
      await onMutate()
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      setRetryingHill(null)
    }
  }

  return (
    <Stack gap="lg">
      <ScrollArea type="auto">
        <Table verticalSpacing="xs" highlightOnHover>
          <Table.Caption>{t('admin.content.ad_ops.koth.table_caption', 'King of the Hill operations')}</Table.Caption>
          <Table.Thead className={tableClasses.thead}>
            <Table.Tr>
              <Table.Th scope="col">{t('admin.content.ad_ops.koth.col_hill', 'Hill')}</Table.Th>
              <Table.Th scope="col">{t('admin.content.ad_ops.koth.col_phase', 'Cycle / reset')}</Table.Th>
              <Table.Th scope="col">{t('admin.content.ad_ops.koth.col_status', 'Health')}</Table.Th>
              <Table.Th scope="col">{t('admin.content.ad_ops.koth.col_king', 'Control')}</Table.Th>
              <Table.Th scope="col">{t('admin.content.ad_ops.koth.col_cooldown', 'Cooldown')}</Table.Th>
              <Table.Th scope="col">{t('admin.content.ad_ops.koth.col_container', 'Container transition')}</Table.Th>
              <Table.Th scope="col">{t('admin.content.ad_ops.koth.col_endpoint', 'Endpoint')}</Table.Th>
              <Table.Th scope="col" w={90}>
                {t('admin.content.ad_ops.koth.col_enabled', 'Enabled')}
              </Table.Th>
              <Table.Th scope="col" w={90}>
                {t('admin.content.ad_ops.koth.col_actions', 'Recovery')}
              </Table.Th>
            </Table.Tr>
          </Table.Thead>
          <Table.Tbody>
            {koth.hills.map((hill) => {
              const health = statusMeta(hill.lastCheckStatus)
              const phase = hill.resetPhase
              const hasCycle = hill.cycleNumber > 0
              const cooldown = hill.cooldownParticipants
              return (
                <Table.Tr key={hill.challengeId} style={{ opacity: hill.isEnabled ? 1 : 0.5 }}>
                  <Table.Td>
                    <Stack gap={2}>
                      <Text fw="bold" size="sm">
                        {hill.title}
                      </Text>
                      <Group gap={4}>
                        {hill.resetReceiptId != null && (
                          <Badge size="xs" color="gray" variant="light">
                            reset #{hill.resetReceiptId}
                          </Badge>
                        )}
                        {hill.scoringReceiptId != null && (
                          <Badge size="xs" color="cyan" variant="light">
                            score #{hill.scoringReceiptId}
                          </Badge>
                        )}
                      </Group>
                    </Stack>
                  </Table.Td>
                  <Table.Td>
                    <Stack gap={3}>
                      <Group gap={4} wrap="wrap">
                        <Badge
                          size="xs"
                          color={
                            !hasCycle
                              ? 'gray'
                              : phase === 'Failed'
                                ? 'red'
                                : phase === 'Active'
                                  ? 'teal'
                                  : phase === 'Ended'
                                    ? 'gray'
                                    : 'orange'
                          }
                          variant={!hasCycle || phase === 'Active' || phase === 'Ended' ? 'light' : 'filled'}
                        >
                          {hasCycle ? phase : t('admin.content.ad_ops.koth.awaiting_cycle', 'Awaiting cycle')}
                        </Badge>
                        {hasCycle && (
                          <Badge size="xs" color="violet" variant="light">
                            C{hill.cycleNumber}
                            {` · ${hill.cycleTick}/${koth.cycleTicks}`}
                          </Badge>
                        )}
                      </Group>
                      <Text size="xs" c="dimmed" className={misc.ffmono}>
                        durable: {hill.durablePhase}
                      </Text>
                      {phase === 'Active' && hill.nextResetTicks != null && (
                        <Text size="xs" c="dimmed">
                          {t('admin.content.ad_ops.koth.reset_in', {
                            count: hill.nextResetTicks,
                            defaultValue: 'Reset in {{count}} tick(s)',
                          })}
                        </Text>
                      )}
                      {hill.readinessFailureCount > 0 && (
                        <Tooltip label={hill.lastReadinessError ?? ''} disabled={!hill.lastReadinessError} withArrow>
                          <Text size="xs" c="red">
                            {t('admin.content.ad_ops.koth.readiness_failures', {
                              count: hill.readinessFailureCount,
                              defaultValue: '{{count}} readiness failure(s)',
                            })}
                          </Text>
                        </Tooltip>
                      )}
                    </Stack>
                  </Table.Td>
                  <Table.Td>
                    <Badge
                      size="sm"
                      color={health.color}
                      variant={hill.lastCheckStatus ? 'light' : 'outline'}
                      leftSection={<Icon path={health.icon} size={0.55} />}
                    >
                      {hill.lastCheckStatus ?? '—'}
                    </Badge>
                  </Table.Td>
                  <Table.Td>
                    <Stack gap={3}>
                      {hill.currentHolderTeamName ? (
                        <Group gap={4} wrap="nowrap">
                          <Icon path={mdiCrown} size={0.65} color="var(--mantine-color-yellow-6)" />
                          <Text size="sm" truncate maw="12rem">
                            {hill.currentHolderTeamName}
                          </Text>
                        </Group>
                      ) : (
                        <Text size="sm" c="dimmed">
                          {t('admin.content.ad_ops.koth.no_king', 'No confirmed king')}
                        </Text>
                      )}
                      {hill.provisionalClaimantTeamName && (
                        <Badge size="xs" color="orange" variant="light" style={{ alignSelf: 'flex-start' }}>
                          {t('admin.content.ad_ops.koth.provisional', {
                            team: hill.provisionalClaimantTeamName,
                            current: hill.provisionalConfirmationTicks,
                            required: koth.claimConfirmationTicks,
                            defaultValue: 'Provisional {{team}} · {{current}}/{{required}}',
                          })}
                        </Badge>
                      )}
                    </Stack>
                  </Table.Td>
                  <Table.Td>
                    {hill.cycleChampions.length > 0 || cooldown.length > 0 ? (
                      <Stack gap={4}>
                        {hill.cycleChampions.length > 0 && (
                          <Stack gap={1}>
                            <Text size="xs" c="dimmed" fw={600}>
                              {t('admin.content.ad_ops.koth.cycle_champions', {
                                count: hill.cycleChampions.length,
                                cycle: hill.cycleChampions[0]?.sourceCycleNumber,
                                defaultValue: 'Previous cycle C{{cycle}} champion(s)',
                              })}
                            </Text>
                            {hill.cycleChampions.map((champion) => (
                              <Text key={`${champion.sourceCycleNumber}:${champion.participationId}`} size="xs">
                                {champion.teamName} · {champion.healthyControlledTicks} healthy tick(s)
                              </Text>
                            ))}
                          </Stack>
                        )}
                        {cooldown.length > 0 && (
                          <Text size="xs" c="dimmed" fw={600}>
                            {t('admin.content.ad_ops.koth.active_cooldown', 'Active cooldown')}
                          </Text>
                        )}
                        {cooldown.map((entry) => (
                          <Text key={entry.participationId} size="xs">
                            {entry.teamName} · {entry.remainingTicks}t
                          </Text>
                        ))}
                      </Stack>
                    ) : (
                      <Text size="xs" c="dimmed">
                        —
                      </Text>
                    )}
                  </Table.Td>
                  <Table.Td>
                    <Stack gap={2}>
                      <CopyId label="old" value={hill.oldContainerId} />
                      <CopyId label="new" value={hill.replacementContainerId} />
                      {!hill.oldContainerId && !hill.replacementContainerId && (
                        <CopyId label="live" value={hill.containerGuid} />
                      )}
                      {hill.resetAttempt > 0 && (
                        <Text size="xs" c="dimmed">
                          attempt {hill.resetAttempt}
                        </Text>
                      )}
                    </Stack>
                  </Table.Td>
                  <Table.Td>
                    {hill.containerIp ? (
                      <Group gap={4} wrap="nowrap">
                        <CopyButton value={`${hill.containerIp}:${hill.containerPort ?? ''}`}>
                          {({ copied, copy }) => (
                            <Tooltip
                              label={
                                copied
                                  ? t('game.tooltip.copy.copied', 'Copied')
                                  : t('game.tooltip.copy.ip_port', 'Copy IP:port')
                              }
                            >
                              <UnstyledButton
                                onClick={copy}
                                aria-label={t('game.tooltip.copy.ip_port', 'Copy IP:port')}
                              >
                                <Text className={misc.ffmono} size="xs">
                                  {hill.containerIp}:{hill.containerPort}
                                </Text>
                              </UnstyledButton>
                            </Tooltip>
                          )}
                        </CopyButton>
                        {hill.containerGuid && (
                          <Tooltip label={t('admin.tooltip.ad_ops.shell', 'Open a shell in this container')} withArrow>
                            <ActionIcon
                              size="sm"
                              variant="subtle"
                              color="blue"
                              aria-label={t('admin.tooltip.ad_ops.shell', 'Open a shell in this container')}
                              onClick={() => onShell(hill.containerGuid!, hill.title)}
                            >
                              <Icon path={mdiConsole} size={0.7} />
                            </ActionIcon>
                          </Tooltip>
                        )}
                      </Group>
                    ) : (
                      <Text size="xs" c="dimmed">
                        {t('admin.content.ad_ops.koth.no_container', 'no active container')}
                      </Text>
                    )}
                  </Table.Td>
                  <Table.Td>
                    <Tooltip
                      label={t('admin.tooltip.ad_ops.koth.toggle_hill', {
                        title: hill.title,
                        defaultValue: 'Enable/disable {{title}} (non-destructive)',
                      })}
                      withArrow
                    >
                      <Switch
                        checked={hill.isEnabled}
                        disabled={busyHill === hill.challengeId}
                        onChange={() => onToggleHill(hill)}
                        aria-label={t('admin.tooltip.ad_ops.koth.toggle_hill', {
                          title: hill.title,
                          defaultValue: 'Enable/disable {{title}} (non-destructive)',
                        })}
                      />
                    </Tooltip>
                  </Table.Td>
                  <Table.Td>
                    <Stack gap={4} align="flex-start">
                      <Button size="compact-xs" variant="subtle" onClick={() => openReceipts(hill)}>
                        {t('admin.button.ad_ops.koth.receipts', 'Receipts')}
                      </Button>
                      <Tooltip
                        label={
                          hill.canRetry
                            ? t(
                                'admin.tooltip.ad_ops.koth.retry',
                                'Resume the durable reset state machine. Repeated calls are safe.'
                              )
                            : t('admin.tooltip.ad_ops.koth.retry_unavailable', 'No interrupted reset needs recovery.')
                        }
                        withArrow
                      >
                        <Button
                          size="compact-xs"
                          color="orange"
                          variant="light"
                          leftSection={<Icon path={mdiRestart} size={0.7} />}
                          loading={retryingHill === hill.challengeId}
                          disabled={!hill.canRetry || (retryingHill != null && retryingHill !== hill.challengeId)}
                          onClick={() => recoverHill(hill)}
                        >
                          {t('admin.button.ad_ops.koth.retry', 'Retry')}
                        </Button>
                      </Tooltip>
                    </Stack>
                  </Table.Td>
                </Table.Tr>
              )
            })}
          </Table.Tbody>
        </Table>
      </ScrollArea>

      {hasResetInProgress && (
        <Alert color="orange" variant="light">
          <Text size="sm">
            {t(
              'admin.content.ad_ops.koth.reset_scoring_note',
              'Checker attribution and scoring remain paused until the replacement container passes readiness. Reset time is void evidence.'
            )}
          </Text>
        </Alert>
      )}

      <Stack gap="xs">
        <Group gap="xs" align="center">
          <Title order={5}>{t('admin.content.ad_ops.koth.leaderboard', 'Official KotH leaderboard')}</Title>
        </Group>
        {koth.teams.length === 0 || enabledHills.length === 0 ? (
          <Text size="sm" c="dimmed">
            {t('admin.content.ad_ops.koth.no_scores', 'No official KotH score yet.')}
          </Text>
        ) : (
          <ScrollArea h="40vh" type="auto">
            <Table verticalSpacing="xs" striped highlightOnHover withColumnBorders>
              <Table.Caption>{t('admin.content.ad_ops.koth.leaderboard', 'Official KotH leaderboard')}</Table.Caption>
              <Table.Thead className={tableClasses.thead}>
                <Table.Tr>
                  <Table.Th scope="col" w={40}>
                    #
                  </Table.Th>
                  <Table.Th scope="col">{t('admin.content.ad_ops.column_team', 'Team')}</Table.Th>
                  <Table.Th scope="col" w={100}>
                    {t('admin.content.ad_ops.koth.total', 'Settled / live')}
                  </Table.Th>
                  {enabledHills.map((hill) => (
                    <Table.Th scope="col" key={hill.challengeId}>
                      <Text truncate fw="bold" size="xs" maw="8rem">
                        {hill.title}
                      </Text>
                    </Table.Th>
                  ))}
                </Table.Tr>
              </Table.Thead>
              <Table.Tbody>
                {koth.teams.map((row) => (
                  <Table.Tr key={row.participationId}>
                    <Table.Td>
                      <Text size="sm" c="dimmed">
                        {row.rank}
                      </Text>
                    </Table.Td>
                    <Table.Td>
                      <Text truncate fw="bold" size="sm" maw="12rem">
                        {row.teamName}
                      </Text>
                    </Table.Td>
                    <Table.Td>
                      <Stack gap={0}>
                        <Text fw="bold" size="sm">
                          {fmtPts(row.settledTotal)}
                        </Text>
                        {Math.abs(row.projectedTotal - row.settledTotal) > 0.05 && (
                          <Text size="xs" c="orange">
                            live {fmtPts(row.projectedTotal)}
                          </Text>
                        )}
                      </Stack>
                    </Table.Td>
                    {enabledHills.map((hill) => {
                      const cell = row.hills.find((score) => score.challengeId === hill.challengeId)
                      return (
                        <Table.Td key={hill.challengeId}>
                          <Group gap={4} wrap="nowrap">
                            <Text size="sm">{cell ? fmtPts(cell.settledPoints) : '0'}</Text>
                            {cell?.isCurrentHolder && (
                              <Icon path={mdiCrown} size={0.55} color="var(--mantine-color-yellow-6)" />
                            )}
                          </Group>
                        </Table.Td>
                      )
                    })}
                  </Table.Tr>
                ))}
              </Table.Tbody>
            </Table>
          </ScrollArea>
        )}
      </Stack>

      <Modal
        opened={auditHill !== null}
        onClose={() => {
          setAuditHill(null)
          setAudit(null)
        }}
        size="xl"
        centered
        title={t('admin.content.ad_ops.koth.receipts_title', {
          hill: auditHill?.title ?? '',
          defaultValue: 'Reset & scoring receipts — {{hill}}',
        })}
      >
        {auditLoading ? (
          <Text size="sm" c="dimmed">
            {t('admin.content.ad_ops.koth.receipts_loading', 'Loading audit receipts…')}
          </Text>
        ) : audit && audit.receipts.length > 0 ? (
          <Stack gap="sm">
            <Group gap="xs">
              <Badge color="violet" variant="light">
                {t('admin.content.ad_ops.koth.receipts_cycle', {
                  cycle: audit.cycleNumber,
                  defaultValue: 'Cycle {{cycle}}',
                })}
              </Badge>
              <Text size="xs" c="dimmed">
                {t('admin.content.ad_ops.koth.receipts_count', {
                  count: audit.receipts.length,
                  defaultValue: '{{count}} receipt(s)',
                })}
              </Text>
            </Group>
            {audit.receipts.map((entry) => (
              <AuditReceiptDetails key={entry.id} entry={entry} />
            ))}
          </Stack>
        ) : (
          <Text size="sm" c="dimmed">
            {t('admin.content.ad_ops.koth.receipts_empty', 'No receipts have been recorded for this hill yet.')}
          </Text>
        )}
      </Modal>
    </Stack>
  )
}
