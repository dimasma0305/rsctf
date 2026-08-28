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
  mdiApi,
  mdiCheck,
  mdiCheckCircle,
  mdiCloseCircle,
  mdiConsole,
  mdiCrown,
  mdiFileOutline,
  mdiHelpCircle,
  mdiKeyVariant,
  mdiRestart,
  mdiTrashCanOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import {
  type KothObserverOperationKind,
  type KothObserverOperationOwner,
  newKothObserverOperationId,
  ownsKothObserverResult,
} from '@Utils/KothObserverOperations'
import { showErrorMsg } from '@Utils/Shared'
import { isKothResetTransition } from '@Utils/kothLifecycle'
import {
  type AdminKothAuditReceipt,
  type AdminKothHill,
  type AdminKothObserverModel,
  type AdminKothReceiptsModel,
  type AdminKothStateModel,
} from '@Hooks/useGame'
import api, { ContentType } from '@Api'
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
const OBSERVER_MUTATION_TIMEOUT_MS = 15_000
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

const KothClaimInputCell: FC<{ hill: AdminKothHill; onOpen: (hill: AdminKothHill) => void }> = ({ hill, onOpen }) => {
  const { t } = useTranslation()
  return (
    <Table.Td>
      <Stack gap={4} align="flex-start">
        <Badge
          size="xs"
          color={hill.claimSource === 'Api' ? (hill.apiObserverConfigured ? 'blue' : 'red') : 'gray'}
          variant="light"
          leftSection={<Icon path={hill.claimSource === 'Api' ? mdiApi : mdiFileOutline} size={0.55} />}
        >
          {hill.claimSource === 'Api'
            ? hill.apiObserverConfigured
              ? t('admin.content.ad_ops.koth.observer_api', 'Leaderboard')
              : t('admin.content.ad_ops.koth.observer_missing', 'Leaderboard disabled')
            : hill.cycleNumber > 0
              ? t('admin.content.ad_ops.koth.observer_marker_locked', 'Container marker · locked')
              : t('admin.content.ad_ops.koth.observer_marker', 'Container marker')}
        </Badge>
        {hill.apiObserverSecretHint && (
          <Text size="xs" c="dimmed" className={misc.ffmono}>
            {hill.apiObserverSecretHint}
          </Text>
        )}
        {hill.apiLastObservationAt != null && (
          <Text size="xs" c="dimmed">
            {t('admin.content.ad_ops.koth.observer_last_seen', {
              time: new Date(hill.apiLastObservationAt).toLocaleString(),
              defaultValue: 'Last input {{time}}',
            })}
          </Text>
        )}
        <Button size="compact-xs" variant="subtle" onClick={() => onOpen(hill)}>
          {hill.claimSource === 'Marker' && hill.cycleNumber > 0
            ? t('admin.button.ad_ops.koth.observer_view', 'View input')
            : hill.apiObserverConfigured
              ? t('admin.button.ad_ops.koth.observer_manage', 'Manage scoring')
              : t('admin.button.ad_ops.koth.observer_enable', 'Enable Leaderboard')}
        </Button>
      </Stack>
    </Table.Td>
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
  const [observerHill, setObserverHill] = useState<AdminKothHill | null>(null)
  const [observer, setObserver] = useState<AdminKothObserverModel | null>(null)
  const [observerLoading, setObserverLoading] = useState(false)
  const [observerBusy, setObserverBusy] = useState(false)
  const [pendingObserverOperation, setPendingObserverOperation] = useState<KothObserverOperationOwner | null>(null)
  const observerHillRef = useRef<AdminKothHill | null>(null)
  const observerViewGenerationRef = useRef(0)
  const observerMutationGenerationRef = useRef(0)
  const observerMutationRef = useRef<KothObserverOperationOwner | null>(null)
  const observerBusyRef = useRef(false)
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
        path: `/api/stateful/edit/games/${gameId}/ad/koth/${hill.challengeId}/recover`,
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

  const observerPath = (hill: Pick<AdminKothHill, 'challengeId'>) =>
    `/api/edit/games/${gameId}/ad/koth/${hill.challengeId}/observer`

  const openObserver = async (hill: AdminKothHill) => {
    const pending = observerMutationRef.current
    if (pending && pending.challengeId !== hill.challengeId) {
      showNotification({
        color: 'yellow',
        icon: <Icon path={mdiAlertCircle} size={1} />,
        message: t(
          'admin.notification.ad_ops.koth.observer_recover_first',
          'Recover the pending referee change on its original hill before managing another credential.'
        ),
      })
      return
    }
    observerHillRef.current = hill
    const viewGeneration = ++observerViewGenerationRef.current
    setObserverHill(hill)
    setObserver(null)
    setObserverLoading(true)
    try {
      const response = await api.request<AdminKothObserverModel>({
        path: observerPath(hill),
        method: 'GET',
        format: 'json',
      })
      if (
        observerHillRef.current?.challengeId === hill.challengeId &&
        observerViewGenerationRef.current === viewGeneration
      ) {
        setObserver(response.data)
      }
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      if (observerViewGenerationRef.current === viewGeneration) setObserverLoading(false)
    }
  }

  const closeObserver = () => {
    observerHillRef.current = null
    observerViewGenerationRef.current += 1
    setObserverHill(null)
    setObserver(null)
    setObserverLoading(false)
  }

  const observerOperationPath = (operation: KothObserverOperationOwner) =>
    `${observerPath({ challengeId: operation.challengeId })}/operations/${operation.operationId}`

  const requestObserverOperation = async (operation: KothObserverOperationOwner): Promise<AdminKothObserverModel> => {
    const response = await api.request<AdminKothObserverModel>({
      path: observerPath({ challengeId: operation.challengeId }),
      method: operation.kind === 'Rotate' ? 'POST' : 'DELETE',
      type: ContentType.Json,
      body: {
        operationId: operation.operationId,
        expectedRevision: operation.expectedRevision,
      },
      timeout: OBSERVER_MUTATION_TIMEOUT_MS,
      format: 'json',
    })
    return response.data
  }

  const recoverObserverOperation = async (operation: KothObserverOperationOwner): Promise<AdminKothObserverModel> => {
    const response = await api.request<AdminKothObserverModel>({
      path: observerOperationPath(operation),
      method: 'GET',
      timeout: OBSERVER_MUTATION_TIMEOUT_MS,
      format: 'json',
    })
    return response.data
  }

  const applyObserverOperation = async (
    operation: KothObserverOperationOwner,
    result: AdminKothObserverModel
  ): Promise<boolean> => {
    if (
      !ownsKothObserverResult(
        observerMutationRef.current,
        result,
        observerHillRef.current?.challengeId ?? null,
        observerViewGenerationRef.current
      ) ||
      observerMutationRef.current?.generation !== operation.generation
    ) {
      return false
    }
    setObserver(result)
    observerMutationRef.current = null
    setPendingObserverOperation(null)
    showNotification({
      color: 'teal',
      icon: <Icon path={operation.kind === 'Rotate' ? mdiKeyVariant : mdiCheck} size={1} />,
      message:
        operation.kind === 'Rotate'
          ? t(
              'admin.notification.ad_ops.koth.observer_rotated',
              result.managedTargetReporting
                ? 'Managed scoring is enabled. The displayed credential is only for legacy external reporting.'
                : 'A new referee secret was created. Copy it now; only this authorized operation can recover it for 24 hours.'
            )
          : t(
              'admin.notification.ad_ops.koth.observer_revoked',
              result.managedTargetReporting
                ? 'Managed scoring was disabled for this hill.'
                : 'The KotH referee secret was revoked.'
            ),
    })
    await onMutate()
    return true
  }

  const refreshObserverMetadata = async () => {
    const hill = observerHillRef.current
    if (!hill) return
    const viewGeneration = observerViewGenerationRef.current
    const response = await api.request<AdminKothObserverModel>({
      path: observerPath(hill),
      method: 'GET',
      format: 'json',
    })
    if (
      observerHillRef.current?.challengeId === hill.challengeId &&
      observerViewGenerationRef.current === viewGeneration
    ) {
      setObserver(response.data)
    }
  }

  const runObserverOperation = async (operation: KothObserverOperationOwner, recoverFirst: boolean) => {
    if (observerBusyRef.current) return
    observerBusyRef.current = true
    setObserverBusy(true)
    operation.viewGeneration = observerViewGenerationRef.current
    try {
      let result: AdminKothObserverModel
      if (recoverFirst) {
        try {
          result = await recoverObserverOperation(operation)
        } catch (recoveryError: any) {
          if (recoveryError?.response?.status !== 404) throw recoveryError
          result = await requestObserverOperation(operation)
        }
      } else {
        try {
          result = await requestObserverOperation(operation)
        } catch {
          try {
            result = await recoverObserverOperation(operation)
          } catch (recoveryError: any) {
            if (recoveryError?.response?.status !== 404) throw recoveryError
            result = await requestObserverOperation(operation)
          }
        }
      }
      await applyObserverOperation(operation, result)
    } catch (error) {
      if ((error as any)?.response?.status === 409 && observerMutationRef.current === operation) {
        observerMutationRef.current = null
        setPendingObserverOperation(null)
        try {
          await refreshObserverMetadata()
        } catch {
          // The original conflict remains the useful operator-facing error.
        }
      }
      showErrorMsg(error, t)
    } finally {
      observerBusyRef.current = false
      setObserverBusy(false)
    }
  }

  const beginObserverOperation = (kind: KothObserverOperationKind) => {
    const hill = observerHillRef.current
    if (!hill || !observer) return
    const pending = observerMutationRef.current
    if (pending) {
      void runObserverOperation(pending, true)
      return
    }
    const operation: KothObserverOperationOwner = {
      challengeId: hill.challengeId,
      expectedRevision: observer.revision,
      generation: ++observerMutationGenerationRef.current,
      operationId: newKothObserverOperationId(),
      kind,
      viewGeneration: observerViewGenerationRef.current,
    }
    observerMutationRef.current = operation
    setPendingObserverOperation(operation)
    void runObserverOperation(operation, false)
  }

  const rotateObserver = () => beginObserverOperation('Rotate')

  const revokeObserver = async () => {
    if (!observerHill || !observer?.configured) return
    if (
      !window.confirm(
        t(
          'admin.confirm.ad_ops.koth.observer_revoke',
          observer.managedTargetReporting
            ? 'Disable managed scoring for this hill? Leaderboard evidence will stop until it is enabled again.'
            : 'Revoke this referee secret? Leaderboard evidence will stop until a new secret is created.'
        )
      )
    )
      return
    beginObserverOperation('Revoke')
  }

  return (
    <Stack gap="lg">
      <ScrollArea type="auto">
        <Table verticalSpacing="xs" highlightOnHover>
          <Table.Caption>{t('admin.content.ad_ops.koth.table_caption', 'King of the Hill operations')}</Table.Caption>
          <Table.Thead className={tableClasses.thead}>
            <Table.Tr>
              <Table.Th scope="col">{t('admin.content.ad_ops.koth.col_hill', 'Hill')}</Table.Th>
              <Table.Th scope="col">{t('admin.content.ad_ops.koth.col_phase', 'Lifecycle / recovery')}</Table.Th>
              <Table.Th scope="col">{t('admin.content.ad_ops.koth.col_status', 'Health')}</Table.Th>
              <Table.Th scope="col">{t('admin.content.ad_ops.koth.col_king', 'Control')}</Table.Th>
              <Table.Th scope="col">{t('admin.content.ad_ops.koth.col_cooldown', 'Cooldown')}</Table.Th>
              <Table.Th scope="col">{t('admin.content.ad_ops.koth.col_container', 'Container transition')}</Table.Th>
              <Table.Th scope="col">{t('admin.content.ad_ops.koth.col_endpoint', 'Endpoint')}</Table.Th>
              <Table.Th scope="col">{t('admin.content.ad_ops.koth.col_claim_input', 'Claim input')}</Table.Th>
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
              const isApiArena = hill.claimSource === 'Api'
              return (
                <Table.Tr key={hill.challengeId}>
                  <Table.Td>
                    <Stack gap={2}>
                      <Group gap={4} wrap="wrap">
                        <Text fw="bold" size="sm">
                          {hill.title}
                        </Text>
                        {!hill.isEnabled && (
                          <Badge size="xs" color="gray" variant="outline">
                            {t('common.content.disabled', 'Disabled')}
                          </Badge>
                        )}
                      </Group>
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
                          {hasCycle
                            ? isApiArena && phase === 'Active'
                              ? t('admin.content.ad_ops.koth.api_health_supervised', 'Health supervised')
                              : isApiArena
                                ? t('admin.content.ad_ops.koth.api_recovery', {
                                    phase,
                                    defaultValue: 'Recovery: {{phase}}',
                                  })
                                : phase
                            : t('admin.content.ad_ops.koth.awaiting_cycle', 'Awaiting lifecycle')}
                        </Badge>
                        {hasCycle && !isApiArena && (
                          <Badge size="xs" color="violet" variant="light">
                            C{hill.cycleNumber}
                            {` · ${hill.cycleTick}/${koth.cycleTicks}`}
                          </Badge>
                        )}
                      </Group>
                      <Text size="xs" c="dimmed" className={misc.ffmono}>
                        {isApiArena
                          ? `lifecycle ${hill.cycleNumber} · runtime attempt ${hill.resetAttempt}`
                          : `durable: ${hill.durablePhase}`}
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
                    {isApiArena ? (
                      <Stack gap={2}>
                        <Badge size="xs" color="blue" variant="light" style={{ alignSelf: 'flex-start' }}>
                          {t('admin.content.ad_ops.koth.api_dense', 'Multi-team arena')}
                        </Badge>
                        <Text size="xs" c="dimmed">
                          {t('admin.content.ad_ops.koth.api_dense_detail', 'Every eligible team may score this tick.')}
                        </Text>
                      </Stack>
                    ) : (
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
                    )}
                  </Table.Td>
                  <Table.Td>
                    {isApiArena ? (
                      <Text size="xs" c="dimmed">
                        {t('admin.content.ad_ops.koth.api_no_cooldown', 'Not used in API scoring')}
                      </Text>
                    ) : hill.cycleChampions.length > 0 || cooldown.length > 0 ? (
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
                  <KothClaimInputCell hill={hill} onOpen={openObserver} />
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
          <Title order={3} size="h5">
            {t('admin.content.ad_ops.koth.leaderboard', 'Official KotH leaderboard')}
          </Title>
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

      <Modal
        opened={observerHill !== null}
        onClose={closeObserver}
        size="lg"
        centered
        title={t('admin.content.ad_ops.koth.observer_title', {
          hill: observerHill?.title ?? '',
          defaultValue: 'Leaderboard scoring — {{hill}}',
        })}
      >
        {observerLoading || !observer ? (
          <Text size="sm" c="dimmed">
            {t('admin.content.ad_ops.koth.observer_loading', 'Loading scoring configuration…')}
          </Text>
        ) : (
          <Stack gap="md">
            <Alert
              color={observer.claimSource === 'Api' && !observer.configured ? 'red' : 'blue'}
              variant="light"
              icon={<Icon path={mdiApi} size={0.9} />}
            >
              <Text size="sm">
                {observer.claimSource === 'Api'
                  ? observer.configured
                    ? t(
                        'admin.content.ad_ops.koth.observer_active',
                        observer.managedTargetReporting
                          ? 'This Leaderboard hill receives finalized-wave evidence directly from its platform-managed target. RSCTF injects a lifecycle-bound credential, binds every snapshot to the current target and round, normalizes each result, and calculates every point.'
                          : 'This Leaderboard hill accepts signed finalized-wave evidence from a compatible external referee. RSCTF binds it to the current scoring round, normalizes every result, and calculates every point.'
                      )
                    : t(
                        'admin.content.ad_ops.koth.observer_required',
                        'This hill is officially locked to Leaderboard scoring but scoring is disabled. Enable it before resuming the event.'
                      )
                  : t(
                      'admin.content.ad_ops.koth.observer_marker_mode',
                      'This boot2root hill currently reads /koth/king. Enabling Leaderboard scoring before the official snapshot selects multi-team evidence scoring; the mode cannot change after scoring starts.'
                    )}
              </Text>
              {observer.claimSource === 'Api' && observer.configured && (
                <Text size="xs" mt={6} c="dimmed">
                  {observer.objectiveCount === null
                    ? t(
                        'admin.content.ad_ops.koth.observer_scheme_pending',
                        'The ordered objective IDs will lock on the first accepted signed snapshot, including an explicit no-wave snapshot.'
                      )
                    : t('admin.content.ad_ops.koth.observer_scheme_locked', {
                        count: observer.objectiveCount,
                        defaultValue:
                          'Objective scheme locked: {{count}} identified, equally normalized components for the event. Credential changes cannot alter it.',
                      })}
                </Text>
              )}
              {observer.objectiveIds && observer.objectiveIds.length > 0 && (
                <Text size="xs" mt={4} className={misc.ffmono} style={{ overflowWrap: 'anywhere' }}>
                  {t('admin.content.ad_ops.koth.observer_objective_ids', {
                    ids: observer.objectiveIds.join(' · '),
                    defaultValue: 'Objectives: {{ids}}',
                  })}
                </Text>
              )}
              {observer.objectiveSchemaHash && (
                <Text size="xs" mt={2} c="dimmed" className={misc.ffmono} style={{ overflowWrap: 'anywhere' }}>
                  {t('admin.content.ad_ops.koth.observer_schema_hash', {
                    hash: observer.objectiveSchemaHash,
                    defaultValue: 'Schema SHA-256: {{hash}}',
                  })}
                </Text>
              )}
            </Alert>

            {observer.secret && (
              <Alert
                color="orange"
                variant="light"
                title={t(
                  'admin.content.ad_ops.koth.secret_recoverable',
                  observer.managedTargetReporting
                    ? 'Optional legacy external credential'
                    : 'Copy and store this credential'
                )}
              >
                <Stack gap="xs">
                  <Text size="sm">
                    {t(
                      'admin.content.ad_ops.koth.secret_recoverable_body',
                      observer.managedTargetReporting
                        ? 'No copy is needed for the managed target. rsctf injects a separate short-lived credential for each target lifecycle. This recoverable HMAC value exists only for deployments that still run a compatible external referee.'
                        : 'This HMAC secret is retained for 24 hours only for this authorized operation, so an ambiguous response can recover the exact same result. Copy it into the independent referee service, never the player-facing arena or client.'
                    )}
                  </Text>
                  <Group gap="xs" wrap="nowrap">
                    <Code block className={misc.ffmono} style={{ flex: 1, overflowWrap: 'anywhere' }}>
                      {observer.secret}
                    </Code>
                    <CopyButton value={observer.secret}>
                      {({ copied, copy }) => (
                        <Button size="xs" variant="default" onClick={copy}>
                          {copied ? t('game.tooltip.copy.copied', 'Copied') : t('common.copy', 'Copy')}
                        </Button>
                      )}
                    </CopyButton>
                  </Group>
                </Stack>
              </Alert>
            )}

            {pendingObserverOperation?.challengeId === observer.challengeId && (
              <Alert color="yellow" variant="light" title={t('common.status.pending', 'Pending operation')}>
                <Text size="sm">
                  {t(
                    'admin.content.ad_ops.koth.observer_recovery_pending',
                    'This credential change has an ambiguous response. Recover the same operation before starting another change.'
                  )}
                </Text>
              </Alert>
            )}

            {!observer.managedTargetReporting && (
              <Stack gap={4}>
                <Text size="xs" fw={700}>
                  {t('admin.content.ad_ops.koth.observer_context_endpoint', '1. Fetch active context')}
                </Text>
                <Code block className={misc.ffmono} style={{ overflowWrap: 'anywhere' }}>
                  GET {observer.contextPath}
                </Code>
                <Text size="xs" fw={700} mt="xs">
                  {t('admin.content.ad_ops.koth.observer_post_endpoint', '2. Submit current evidence')}
                </Text>
                <Code block className={misc.ffmono} style={{ whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }}>
                  {`POST ${observer.observationPath}
X-RSCTF-Timestamp: <Unix milliseconds>
X-RSCTF-Signature: sha256=<HMAC-SHA256>

{"context":"<context>","objectiveIds":["official-score"],"waves":[{"waveId":"heat-42","endedAtUnixMs":1786200000000,"teams":[{"tokenHash":"<sha256-current-capability>","activity":{"earned":1,"possible":1},"objectives":[{"earned":150,"possible":150}],"isCrown":true}]}]}`}
                </Code>
                <Text size="xs" c="dimmed">
                  {t(
                    'admin.content.ad_ops.koth.observer_signature',
                    'Hash each current capability with SHA-256 and sign the exact raw body as timestamp.gameId.challengeId.body. Every wave needs a unique ID, its server-confirmed end time, completed native results, and at most one best-scoring Crown. Stable ordered objectiveIds are frozen and bound into later contexts; raw capabilities and platform points are never accepted. Requests expire after five minutes and accepted signatures cannot be replayed.'
                  )}
                </Text>
              </Stack>
            )}

            {observer.managedTargetReporting && observer.configured && (
              <Stack gap={4}>
                <Text size="sm" c="dimmed">
                  {t(
                    'admin.content.ad_ops.koth.managed_target_reporting',
                    'Each newly created target receives scoped context and observation URLs plus a lifecycle-bound signing credential. Challenge code submits native evidence; it never submits platform points.'
                  )}
                </Text>
                <Text size="xs" c="dimmed">
                  {t(
                    'admin.content.ad_ops.koth.managed_target_reset_note',
                    'If scoring was enabled after the current target started, reset that target once so rsctf can inject its managed credential.'
                  )}
                </Text>
              </Stack>
            )}

            <Group justify="space-between" wrap="wrap">
              <Button
                color="red"
                variant="subtle"
                leftSection={<Icon path={mdiTrashCanOutline} size={0.8} />}
                disabled={!observer.configured || pendingObserverOperation !== null}
                loading={observerBusy}
                onClick={revokeObserver}
              >
                {t('admin.button.ad_ops.koth.observer_revoke', 'Disable Leaderboard')}
              </Button>
              <Button
                leftSection={<Icon path={mdiKeyVariant} size={0.8} />}
                disabled={observer.claimSource === 'Marker' && (observerHill?.cycleNumber ?? 0) > 0}
                loading={observerBusy}
                onClick={rotateObserver}
              >
                {pendingObserverOperation
                  ? t('admin.button.ad_ops.koth.observer_recover', 'Recover pending change')
                  : observer.configured
                    ? t(
                        'admin.button.ad_ops.koth.observer_rotate',
                        observer.managedTargetReporting ? 'Rotate legacy fallback' : 'Rotate secret'
                      )
                    : t(
                        'admin.button.ad_ops.koth.observer_create',
                        observer.managedTargetReporting ? 'Enable managed scoring' : 'Create referee secret'
                      )}
              </Button>
            </Group>
          </Stack>
        )}
      </Modal>
    </Stack>
  )
}
