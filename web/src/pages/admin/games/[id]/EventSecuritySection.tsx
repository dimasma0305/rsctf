import {
  Button,
  Divider,
  Group,
  NumberInput,
  Paper,
  SimpleGrid,
  Stack,
  Switch,
  Text,
  Textarea,
  Title,
} from '@mantine/core'
import { useModals } from '@mantine/modals'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { Dispatch, FC, SetStateAction, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { controlJobResultCount, createOperationId, waitForControlJob } from '@Utils/ControlJobs'
import { showErrorMsg } from '@Utils/Shared'
import api, { EventVpnOverrideModel, GameInfoModel } from '@Api'

interface EventSecuritySectionProps {
  disabled: boolean
  game: GameInfoModel | undefined
  isAdmin: boolean
  setGame: Dispatch<SetStateAction<GameInfoModel | undefined>>
  vpnPolicyChanged: boolean
}

export const EventSecuritySection: FC<EventSecuritySectionProps> = ({
  disabled,
  game,
  isAdmin,
  setGame,
  vpnPolicyChanged,
}) => {
  const { t } = useTranslation()
  const modals = useModals()
  const [generatingVariants, setGeneratingVariants] = useState(false)
  const [eventSecurityAction, setEventSecurityAction] = useState<string | null>(null)
  const variantJobRef = useRef<Promise<void> | null>(null)
  const controlJobAbortRef = useRef(new AbortController())
  const deriveInFlight = useRef(false)
  const deriveOperationId = useRef(crypto.randomUUID())
  const [vpnOverrides, setVpnOverrides] = useState<EventVpnOverrideModel[]>([])
  const [vpnPolicyRevision, setVpnPolicyRevision] = useState<number>(1)
  const vpnMutationOwner = useRef(false)
  const createOverrideOperation = useRef<string | null>(null)
  const revokeOverrideOperations = useRef(new Map<string, string>())
  const [overrideReason, setOverrideReason] = useState('')
  const [overrideMinutes, setOverrideMinutes] = useState<number | string>(15)
  const [purgeReason, setPurgeReason] = useState('')

  useEffect(() => () => controlJobAbortRef.current.abort(), [])

  useEffect(() => {
    let cancelled = false
    if (!isAdmin || !game?.id) {
      setVpnOverrides([])
      return
    }
    api.eventSecurity
      .listVpnOverrides(game.id)
      .then((response) => {
        if (!cancelled) {
          setVpnOverrides(response.data.overrides)
          setVpnPolicyRevision(response.data.policyRevision)
        }
      })
      .catch(() => {
        if (!cancelled) setVpnOverrides([])
      })
    return () => {
      cancelled = true
    }
  }, [game?.id, isAdmin])

  const onGenerateVariants = async () => {
    if (!game?.id) return
    if (variantJobRef.current) return variantJobRef.current
    const gameId = game.id
    const operationId = createOperationId()
    const task = (async () => {
      setGeneratingVariants(true)
      try {
        let job
        try {
          job = (await api.eventSecurity.generateVariants(gameId, operationId)).data
        } catch (startError) {
          try {
            job = (await api.eventSecurity.getControlJobByOperation(operationId)).data
          } catch {
            throw startError
          }
        }
        const completed = await waitForControlJob(job, controlJobAbortRef.current.signal)
        showNotification({
          color: 'teal',
          message: t('admin.event_security.variants_generated', '{{count}} deterministic variants generated', {
            count: controlJobResultCount(completed, 'generated'),
          }),
          icon: <Icon path={mdiCheck} size={1} />,
        })
      } catch (error) {
        if (!(error instanceof DOMException && error.name === 'AbortError')) showErrorMsg(error, t)
      } finally {
        setGeneratingVariants(false)
        variantJobRef.current = null
      }
    })()
    variantJobRef.current = task
    return task
  }

  const onDeriveFindings = async () => {
    if (!game?.id || deriveInFlight.current) return
    deriveInFlight.current = true
    setEventSecurityAction('derive')
    try {
      const response = await api.eventSecurity.deriveFindings(game.id, deriveOperationId.current)
      if (response.data.status === 'Completed') deriveOperationId.current = crypto.randomUUID()
      showNotification({
        color: 'teal',
        message: t('admin.event_security.findings_derived', '{{count}} new context findings derived', {
          count: response.data.inserted,
        }),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      deriveInFlight.current = false
      setEventSecurityAction(null)
    }
  }

  const onCreateVpnOverride = async () => {
    if (!game?.id || vpnMutationOwner.current) return
    const reason = overrideReason.trim()
    const durationMinutes = Number(overrideMinutes)
    if (
      reason.length < 8 ||
      reason.length > 512 ||
      !Number.isInteger(durationMinutes) ||
      durationMinutes < 1 ||
      durationMinutes > 60
    ) {
      showNotification({
        color: 'orange',
        message: t(
          'admin.event_security.override_invalid',
          'Enter an 8–512 character reason and a duration from 1 to 60 minutes.'
        ),
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }
    vpnMutationOwner.current = true
    setEventSecurityAction('override')
    const operationId = createOverrideOperation.current ?? crypto.randomUUID()
    createOverrideOperation.current = operationId
    try {
      await api.eventSecurity.createVpnOverride(game.id, {
        reason,
        durationMinutes,
        operationId,
        expectedPolicyRevision: vpnPolicyRevision,
      })
      const refreshed = await api.eventSecurity.listVpnOverrides(game.id)
      setVpnOverrides(refreshed.data.overrides)
      setVpnPolicyRevision(refreshed.data.policyRevision)
      createOverrideOperation.current = null
      setOverrideReason('')
      showNotification({
        color: 'orange',
        message: t('admin.event_security.override_created', 'Temporary event VPN bypass created and audited.'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      vpnMutationOwner.current = false
      setEventSecurityAction(null)
    }
  }

  const onRevokeVpnOverride = async (overrideId: string) => {
    if (!game?.id || vpnMutationOwner.current) return
    vpnMutationOwner.current = true
    setEventSecurityAction(`revoke:${overrideId}`)
    const operationId = revokeOverrideOperations.current.get(overrideId) ?? crypto.randomUUID()
    revokeOverrideOperations.current.set(overrideId, operationId)
    try {
      await api.eventSecurity.revokeVpnOverride(game.id, overrideId, {
        operationId,
        expectedPolicyRevision: vpnPolicyRevision,
      })
      const refreshed = await api.eventSecurity.listVpnOverrides(game.id)
      setVpnOverrides(refreshed.data.overrides)
      setVpnPolicyRevision(refreshed.data.policyRevision)
      revokeOverrideOperations.current.delete(overrideId)
      showNotification({
        color: 'teal',
        message: t('admin.event_security.override_revoked', 'Event VPN bypass revoked.'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      vpnMutationOwner.current = false
      setEventSecurityAction(null)
    }
  }

  const onPurgeTelemetry = async () => {
    if (!game?.id) return
    const reason = purgeReason.trim()
    if (reason.length < 8 || reason.length > 512) {
      showNotification({
        color: 'orange',
        message: t('admin.event_security.purge_reason_invalid', 'Enter an 8–512 character purge reason.'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }
    setEventSecurityAction('purge')
    try {
      const response = await api.eventSecurity.purgeTelemetry(game.id, { reason })
      setPurgeReason('')
      showNotification({
        color: 'teal',
        message: t(
          'admin.event_security.telemetry_purged',
          'Purged {{rows}} raw rows ({{bytes}} logical bytes); immutable findings were retained.',
          { rows: response.data.rowsRemoved, bytes: response.data.logicalBytesRemoved }
        ),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      setEventSecurityAction(null)
    }
  }

  return (
    <Stack gap="md">
      <Title order={2}>{t('admin.event_security.section', 'Event security')}</Title>
      <Text size="sm" c="dimmed">
        {t(
          'admin.event_security.description',
          'Every switch is opt-in. Context telemetry never proves cheating by itself, packet payloads and DNS names are not stored, and event/global quotas fail open for gameplay.'
        )}
      </Text>
      <Divider />
      <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="md">
        <Switch
          disabled={disabled}
          checked={game?.vpnAccessRequired ?? false}
          label={t('admin.event_security.vpn_required', 'Require event VPN for player APIs')}
          description={t(
            'admin.event_security.vpn_required_description',
            'Accepted players receive individual WireGuard credentials and must prove tunnel presence during the active event.'
          )}
          onChange={(event) =>
            game &&
            setGame({
              ...game,
              vpnAccessRequired: event.currentTarget.checked,
              ...(!event.currentTarget.checked
                ? {
                    vpnBehaviorTelemetryEnabled: false,
                    vpnFlagScanEnabled: false,
                    vpnProviderDnsTelemetryEnabled: false,
                    vpnSourceAsnTelemetryEnabled: false,
                    vpnDeviceSharingTelemetryEnabled: false,
                  }
                : {}),
            })
          }
        />
        <Switch
          disabled={disabled || !game?.vpnAccessRequired}
          checked={game?.vpnBehaviorTelemetryEnabled ?? false}
          label={t('admin.event_security.flow', 'Aggregate VPN flow telemetry')}
          description={t(
            'admin.event_security.flow_description',
            'Five-minute byte, packet, connection, and destination-count buckets; no packet content or destination address is stored.'
          )}
          onChange={(event) => game && setGame({ ...game, vpnBehaviorTelemetryEnabled: event.currentTarget.checked })}
        />
        <Switch
          disabled={disabled || !game?.vpnAccessRequired}
          checked={game?.vpnFlagScanEnabled ?? false}
          label={t('admin.event_security.flag_scan', 'Exact foreign-flag transport matching')}
          description={t(
            'admin.event_security.flag_scan_description',
            'Matches only real platform-issued flags in bounded memory and persists an HMAC, never the flag text.'
          )}
          onChange={(event) => game && setGame({ ...game, vpnFlagScanEnabled: event.currentTarget.checked })}
        />
        <Switch
          disabled={disabled || !game?.vpnAccessRequired}
          checked={game?.vpnProviderDnsTelemetryEnabled ?? false}
          label={t('admin.event_security.provider_dns', 'AI/hosting DNS categories')}
          description={t(
            'admin.event_security.provider_dns_description',
            'Counts coarse provider categories only when DNS traffic crosses the event VPN. This is context, not proof of AI use.'
          )}
          onChange={(event) =>
            game && setGame({ ...game, vpnProviderDnsTelemetryEnabled: event.currentTarget.checked })
          }
        />
        <Switch
          disabled={disabled || !game?.vpnAccessRequired}
          checked={game?.vpnSourceAsnTelemetryEnabled ?? false}
          label={t('admin.event_security.source_network', 'Peer source-network class')}
          description={t(
            'admin.event_security.source_network_description',
            'Stores a keyed endpoint hash plus coarse ISP/VPS/VPN class; never stores the public endpoint.'
          )}
          onChange={(event) => game && setGame({ ...game, vpnSourceAsnTelemetryEnabled: event.currentTarget.checked })}
        />
        <Switch
          disabled={disabled || !game?.vpnAccessRequired}
          checked={game?.vpnDeviceSharingTelemetryEnabled ?? false}
          label={t('admin.event_security.device_sharing', 'Multi-endpoint peer context')}
          description={t(
            'admin.event_security.device_sharing_description',
            'Flags one personal event profile appearing from multiple keyed endpoint identities. It remains zero-score context.'
          )}
          onChange={(event) =>
            game && setGame({ ...game, vpnDeviceSharingTelemetryEnabled: event.currentTarget.checked })
          }
        />
      </SimpleGrid>
      {vpnPolicyChanged && (
        <Textarea
          required
          minRows={2}
          maxLength={512}
          label={t('admin.event_security.change_reason', 'Policy change reason')}
          description={t(
            'admin.event_security.change_reason_description',
            'Required for the append-only policy audit (8–512 characters).'
          )}
          value={game?.vpnPolicyChangeReason ?? ''}
          onChange={(event) => game && setGame({ ...game, vpnPolicyChangeReason: event.currentTarget.value })}
        />
      )}
      <Paper withBorder p="md" radius="md">
        <Group justify="space-between" align="flex-start" wrap="wrap">
          <Stack gap={2} style={{ flex: '1 1 24rem' }}>
            <Text fw={600}>{t('admin.event_security.variants', 'Deterministic team variants')}</Text>
            <Text size="xs" c="dimmed">
              {t(
                'admin.event_security.variants_description',
                'Runs each configured generator twice in a network-disabled, resource-limited container and freezes output only when both hashes match.'
              )}
            </Text>
          </Stack>
          <Button
            variant="outline"
            loading={generatingVariants}
            disabled={disabled || generatingVariants}
            onClick={onGenerateVariants}
          >
            {t('admin.event_security.generate_variants', 'Generate missing variants')}
          </Button>
        </Group>
      </Paper>
      {isAdmin && (
        <Paper withBorder p="md" radius="md">
          <Stack gap="md">
            <Stack gap={2}>
              <Text fw={600}>{t('admin.event_security.operations', 'Investigation and recovery')}</Text>
              <Text size="xs" c="dimmed">
                {t(
                  'admin.event_security.operations_description',
                  'Derivation is idempotent. Emergency bypasses are short-lived and append-only audited; they do not disable telemetry collection.'
                )}
              </Text>
            </Stack>
            <Button
              variant="outline"
              loading={eventSecurityAction === 'derive'}
              disabled={disabled || eventSecurityAction !== null}
              onClick={onDeriveFindings}
            >
              {t('admin.event_security.derive_findings', 'Derive bounded context findings')}
            </Button>
            <Divider />
            <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="md">
              <Textarea
                minRows={2}
                maxLength={512}
                label={t('admin.event_security.override_reason', 'Emergency bypass reason')}
                value={overrideReason}
                onChange={(event) => setOverrideReason(event.currentTarget.value)}
              />
              <NumberInput
                min={1}
                max={60}
                allowDecimal={false}
                label={t('admin.event_security.override_minutes', 'Duration (minutes)')}
                value={overrideMinutes}
                onChange={setOverrideMinutes}
              />
            </SimpleGrid>
            <Button
              color="orange"
              variant="outline"
              loading={eventSecurityAction === 'override'}
              disabled={disabled || eventSecurityAction !== null}
              onClick={onCreateVpnOverride}
            >
              {t('admin.event_security.create_override', 'Create temporary VPN bypass')}
            </Button>
            {vpnOverrides
              .filter((item) => item.active)
              .map((item) => (
                <Group key={item.id} justify="space-between" align="flex-start" wrap="wrap">
                  <Stack gap={0} style={{ flex: '1 1 20rem' }}>
                    <Text size="sm" fw={600}>
                      {t('admin.event_security.active_override', 'Active until {{time}}', {
                        time: dayjs(item.expiresAtUtc).format('L LT'),
                      })}
                    </Text>
                    <Text size="xs" c="dimmed">
                      {item.reason}
                    </Text>
                  </Stack>
                  <Button
                    size="xs"
                    color="red"
                    variant="outline"
                    loading={eventSecurityAction === `revoke:${item.id}`}
                    disabled={disabled || eventSecurityAction !== null}
                    onClick={() => onRevokeVpnOverride(item.id)}
                  >
                    {t('admin.event_security.revoke_override', 'Revoke bypass')}
                  </Button>
                </Group>
              ))}
          </Stack>
        </Paper>
      )}
      {isAdmin && (
        <Paper withBorder p="md" radius="md">
          <Stack gap="sm">
            <Text fw={600}>{t('admin.event_security.purge', 'Purge raw event telemetry')}</Text>
            <Text size="xs" c="dimmed">
              {t(
                'admin.event_security.purge_description',
                'Deletes bounded flow/DNS/network/flag-transport rows for this event. Findings, relationships, reviews, and the purge audit remain.'
              )}
            </Text>
            <Textarea
              minRows={2}
              maxLength={512}
              label={t('admin.event_security.purge_reason', 'Purge reason')}
              value={purgeReason}
              onChange={(event) => setPurgeReason(event.currentTarget.value)}
            />
            <Button
              color="red"
              variant="outline"
              loading={eventSecurityAction === 'purge'}
              disabled={disabled || eventSecurityAction !== null}
              onClick={() =>
                modals.openConfirmModal({
                  title: t('admin.event_security.purge_confirm_title', 'Purge raw event telemetry?'),
                  children: (
                    <Text size="sm">
                      {t(
                        'admin.event_security.purge_confirm_body',
                        'This cannot be undone. Immutable findings and review history will be retained.'
                      )}
                    </Text>
                  ),
                  confirmProps: { color: 'red' },
                  onConfirm: () => void onPurgeTelemetry(),
                })
              }
            >
              {t('admin.event_security.purge_action', 'Purge raw telemetry')}
            </Button>
          </Stack>
        </Paper>
      )}
    </Stack>
  )
}
