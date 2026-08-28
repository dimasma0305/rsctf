import { Alert, Badge, Button, CopyButton, Group, Loader, Stack, Text, Tooltip } from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiAlertCircleOutline, mdiConsole, mdiRefresh, mdiRestart, mdiServerNetwork } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useCallback, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { SnapshotDownloadButton } from '@Components/SnapshotDownloadButton'
import { assertJsonResponse } from '@Utils/ChallengePolling'
import { showErrorMsg } from '@Utils/Shared'
import { createOperationId, waitForControlJob } from '@Utils/ControlJobs'
import { useChallengePolling } from '@Hooks/useChallengePolling'
import api, { AdSshKeyInfoModel, AdStateModel, AdTeamServiceStateModel } from '@Api'
import misc from '@Styles/Misc.module.css'

const statusColor = (s?: string | null) => {
  switch (s) {
    case 'Ok':
      return 'teal'
    case 'Mumble':
      return 'yellow'
    case 'Offline':
      return 'red'
    case 'InternalError':
      return 'gray'
    default:
      return 'gray'
  }
}

interface AdChallengePanelProps {
  gameId: number
  challengeId: number
  active: boolean
  /**
   * Render ONLY the post-game snapshot (service backup) download, hiding the
   * live defending/SSH/reset state. Used after the game ends in practice mode,
   * where the challenge is shown as a standard practice container but the team's
   * defended-service backup must still be downloadable.
   */
  snapshotOnly?: boolean
}

/**
 * Per-challenge A&amp;D status block: container IP+port, the current flag the
 * team should defend, the latest health-check verdict, and a reset-to-baseline
 * button. The token-management UI and the API/curl docs live in the A&amp;D
 * Toolkit modal (sidebar button) so this panel only shows live per-team
 * operational state.
 */
export const AdChallengePanel: FC<AdChallengePanelProps> = ({ gameId, challengeId, active, snapshotOnly }) => {
  const { t } = useTranslation()
  const stateRequest = useCallback(
    async (signal: AbortSignal) => {
      const response = await api.game.gameAdState(gameId, { signal })
      return assertJsonResponse(response)
    },
    [gameId]
  )
  const {
    data: adState,
    error: stateError,
    mutate: mutateState,
  } = useChallengePolling<AdStateModel>({
    key: gameId > 0 ? `/api/Game/${gameId}/Ad/State` : null,
    active,
    refreshInterval: snapshotOnly ? 0 : 10_000,
    request: stateRequest,
  })
  const sshRequest = useCallback(
    async (signal: AbortSignal) => {
      const response = await api.game.adGameGetSshKey(gameId, { signal })
      return assertJsonResponse(response)
    },
    [gameId]
  )
  const { data: sshKey } = useChallengePolling<AdSshKeyInfoModel>({
    key: gameId > 0 ? `/api/Game/${gameId}/Ad/Ssh/Key` : null,
    active: active && !snapshotOnly,
    refreshInterval: 0,
    request: sshRequest,
  })
  const [resetting, setResetting] = useState(false)
  const resetPromiseRef = useRef<Promise<void> | null>(null)
  const resetAbortRef = useRef<AbortController | null>(null)
  useEffect(() => () => resetAbortRef.current?.abort(), [])

  const service: AdTeamServiceStateModel | undefined = adState?.services.find((s) => s.challengeId === challengeId)

  // The team's post-game service backup (the defended container, as a loadable
  // Docker image). Stays available after the game ends so players can keep it.
  const snapshotDownload =
    service && service.snapshotAvailable ? (
      <Group gap={6} align="center" wrap="nowrap">
        <Text size="xs" c="dimmed">
          {t('game.content.ad.snapshot', 'Post-game snapshot')}:
        </Text>
        <Tooltip label={t('game.tooltip.ad.snapshot', 'Download the final container filesystem as a TAR archive.')}>
          <SnapshotDownloadButton
            url={api.game.gameAdDownloadSnapshotUrl(gameId, service.adTeamServiceId)}
            filename={`ad-snapshot-service${service.adTeamServiceId}.tar.gz`}
            downloadKey={`player:snapshot:${gameId}:${service.adTeamServiceId}`}
            label={t('game.button.ad.download_snapshot', 'Download .tar.gz')}
            size="compact-xs"
            variant="light"
          />
        </Tooltip>
      </Group>
    ) : null

  const stateFailure = stateError ? (
    <Alert
      icon={<Icon path={mdiAlertCircleOutline} size={0.9} aria-hidden="true" />}
      color={adState ? 'orange' : 'red'}
      variant="light"
      role="alert"
    >
      <Stack gap="xs">
        <Text size="sm">
          {adState
            ? t(
                'game.content.ad.state_refresh_error',
                'The A&D service information could not be refreshed. Showing the last available data.'
              )
            : t('game.content.ad.state_load_error', 'The A&D service information could not be loaded.')}
        </Text>
        <Group>
          <Button
            size="compact-xs"
            variant="light"
            leftSection={<Icon path={mdiRefresh} size={0.7} aria-hidden="true" />}
            aria-label={t('game.button.ad.retry_state', 'Retry A&D state')}
            onClick={() => void mutateState()}
          >
            {t('common.button.retry', 'Retry')}
          </Button>
        </Group>
      </Stack>
    </Alert>
  ) : null

  // Post-end practice: the challenge is shown as a standard container, but the
  // team's service backup must still be reachable — render just that.
  if (snapshotOnly) {
    if (!stateFailure) return snapshotDownload
    return (
      <Stack gap="xs">
        {stateFailure}
        {snapshotDownload}
      </Stack>
    )
  }

  // Render the `ssh <id>@host -p <port>` snippet the player runs to shell
  // into their container for THIS challenge. Host/port come from the SSH
  // key info endpoint (operator-configured Ad:Ssh:PublicHost/Port). We
  // only show the snippet once they've registered a key — otherwise it
  // would just confuse a player whose first auth would fail anyway.
  const renderSshHint = () => {
    if (!sshKey?.jumpHost) return null
    const [host, port] = sshKey.jumpHost.split(':')
    const cmd = `ssh ${challengeId}@${host} -p ${port ?? '22022'}`
    return (
      <Group gap={6} align="center" wrap="nowrap">
        <Tooltip
          label={
            sshKey.exists
              ? t('game.tooltip.ad.ssh_ready', 'SSH key is registered — connect any time')
              : t('game.tooltip.ad.ssh_not_ready', 'Register an SSH key in the Toolkit first')
          }
        >
          <Group gap={4} wrap="nowrap" style={{ opacity: sshKey.exists ? 1 : 0.5 }}>
            <Icon path={mdiConsole} size={0.6} />
            <Text size="xs" c="dimmed">
              SSH:
            </Text>
          </Group>
        </Tooltip>
        <CopyButton value={cmd}>
          {({ copied, copy }) => (
            <Tooltip
              label={
                copied ? t('game.tooltip.copy.copied', 'Copied') : t('game.tooltip.copy.ssh_cmd', 'Copy ssh command')
              }
            >
              <Text
                className={misc.ffmono}
                size="xs"
                c={sshKey.exists ? undefined : 'dimmed'}
                truncate
                role="button"
                tabIndex={0}
                aria-label={t('game.tooltip.copy.ssh_cmd', 'Copy ssh command')}
                style={{ cursor: 'pointer' }}
                onClick={copy}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault()
                    copy()
                  }
                }}
              >
                {cmd}
              </Text>
            </Tooltip>
          )}
        </CopyButton>
      </Group>
    )
  }

  const onReset = async () => {
    if (!service) return
    if (resetPromiseRef.current) return resetPromiseRef.current
    const operationId = createOperationId()
    const controller = new AbortController()
    resetAbortRef.current?.abort()
    resetAbortRef.current = controller
    setResetting(true)
    const request = (async () => {
      try {
        let job
        try {
          job = (
            await api.game.gameAdResetService(gameId, service.adTeamServiceId, operationId, {
              signal: controller.signal,
            })
          ).data
        } catch (error) {
          if (controller.signal.aborted) throw error
          job = (
            await api.game.gameAdResetJobByOperation(gameId, operationId, { signal: controller.signal })
          ).data
        }
        await waitForControlJob(
          job,
          controller.signal,
          async (jobId, signal) => (await api.game.gameAdResetJob(gameId, jobId, { signal })).data
        )
        showNotification({
          color: 'teal',
          icon: <Icon path={mdiRestart} size={1} />,
          title: t('game.notification.ad.reset_queued.title', 'Reset queued'),
          message: t('game.notification.ad.reset_queued.message', 'Container will rebuild in seconds.'),
        })
        await mutateState()
      } catch (e) {
        if (!(e instanceof DOMException && e.name === 'AbortError')) showErrorMsg(e, t)
      }
    })()
    resetPromiseRef.current = request
    try {
      await request
    } finally {
      if (resetPromiseRef.current === request) resetPromiseRef.current = null
      if (resetAbortRef.current === controller) resetAbortRef.current = null
      setResetting(false)
    }
  }

  if (!adState) {
    if (stateFailure) return stateFailure
    return (
      <Group justify="center" py="md">
        <Loader size="sm" />
      </Group>
    )
  }

  if (!service) {
    const noService = (
      <Alert
        icon={<Icon path={mdiAlertCircleOutline} size={1} aria-hidden="true" />}
        color="orange"
        title={t('game.content.ad.no_service.title', 'No service for your team yet')}
      >
        {t(
          'game.content.ad.no_service.description',
          'If you expected a container here, it hasn\'t been provisioned yet. Ask the operator to run "Ensure containers" from the A&D Ops console.'
        )}
      </Alert>
    )
    if (!stateFailure) return noService
    return (
      <Stack gap="xs">
        {stateFailure}
        {noService}
      </Stack>
    )
  }

  return (
    <Stack gap={4}>
      {stateFailure}
      <Group justify="space-between" wrap="nowrap" align="center">
        <Group gap="xs" wrap="nowrap">
          <Text fw="bold" size="sm">
            {t('game.content.ad.defend_target', 'Your service')}
          </Text>
          <Badge
            size="sm"
            color={statusColor(service.lastCheckStatus)}
            variant={service.lastCheckStatus ? 'filled' : 'light'}
          >
            {service.lastCheckStatus ?? t('game.content.ad.no_checks_yet', 'no checks yet')}
          </Badge>
        </Group>
        {/* Reset rebuilds an RSCTF-hosted container. For self-hosted (BYOC) the
            real container lives on the team's machine — they reset it there — so
            the relay reset would only confuse; hide it. */}
        {!service.selfHosted && (
          <Tooltip
            label={
              !service.canReset && service.resetCooldownSecondsRemaining
                ? t('game.tooltip.ad.reset_cooldown', {
                    seconds: service.resetCooldownSecondsRemaining,
                    defaultValue: 'On cooldown — {{seconds}}s remaining',
                  })
                : t('game.tooltip.ad.reset', 'Rebuild this container to baseline (you lose SLA during the rebuild)')
            }
          >
            <Button
              size="compact-xs"
              variant="default"
              leftSection={<Icon path={mdiRestart} size={0.7} />}
              loading={resetting}
              disabled={!service.canReset}
              onClick={onReset}
            >
              {!service.canReset && service.resetCooldownSecondsRemaining
                ? `${service.resetCooldownSecondsRemaining}s`
                : t('game.button.ad.reset', 'Reset')}
            </Button>
          </Tooltip>
        )}
      </Group>

      {service.selfHosted && (
        <Alert icon={<Icon path={mdiServerNetwork} size={1} />} color="blue" variant="light" p="xs">
          <Stack gap={6}>
            <Text size="xs">
              {t(
                'game.content.ad.byoc.description',
                'Self-hosted challenge — run it on a dedicated machine. Download setup.sh and run `sh setup.sh`: it pulls the service and connects with one outbound connection. The script contains team credentials; keep it private and delete the downloaded copy after setup.'
              )}
            </Text>
            <Group gap="xs">
              <Button
                component="a"
                href={`/api/Game/${gameId}/Ad/Byoc/Setup/${challengeId}`}
                download
                size="compact-xs"
                variant="light"
                leftSection={<Icon path={mdiDownload} size={0.7} />}
              >
                {t('game.button.ad.byoc.download', 'Download setup.sh')}
              </Button>
              <Tooltip
                label={t(
                  'game.tooltip.ad.byoc.byo',
                  'Prefer to run your own modified service instead of the one we ship? Get a docker-compose to fill in.'
                )}
              >
                <Button
                  component="a"
                  href={`/api/Game/${gameId}/Ad/Byoc/Compose/${challengeId}`}
                  download
                  size="compact-xs"
                  variant="subtle"
                  color="gray"
                >
                  {t('game.button.ad.byoc.byo', 'Bring your own service')}
                </Button>
              </Tooltip>
            </Group>
          </Stack>
        </Alert>
      )}

      {service.containerIp && (
        <Group gap={6} align="center" wrap="nowrap">
          <Text size="xs" c="dimmed">
            {t('game.content.ad.target', 'Target')}:
          </Text>
          <CopyButton value={`${service.containerIp}:${service.containerPort ?? ''}`}>
            {({ copied, copy }) => (
              <Tooltip
                label={
                  copied ? t('game.tooltip.copy.copied', 'Copied') : t('game.tooltip.copy.ip_port', 'Copy IP:port')
                }
              >
                <Text
                  className={misc.ffmono}
                  size="sm"
                  role="button"
                  tabIndex={0}
                  aria-label={t('game.tooltip.copy.ip_port', 'Copy IP:port')}
                  style={{ cursor: 'pointer' }}
                  onClick={copy}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' || e.key === ' ') {
                      e.preventDefault()
                      copy()
                    }
                  }}
                >
                  {service.containerIp}:{service.containerPort}
                </Text>
              </Tooltip>
            )}
          </CopyButton>
        </Group>
      )}

      {!adState.flagsReady && adState.currentRound > 0 && (
        <Group gap={6} align="center" wrap="nowrap" role="status" aria-live="polite">
          <Loader size="xs" color="yellow" />
          <Text size="xs" c="yellow.7">
            {t(
              'game.content.ad.flags_syncing.description',
              'This round’s flags are still syncing. Wait before attacking or updating your defended flag.'
            )}
          </Text>
        </Group>
      )}

      {adState.flagsReady && adState.flagDeliveryFailures > 0 && (
        <Alert color="orange" icon={<Icon path={mdiAlertCircleOutline} size={0.9} />} role="status">
          {t('game.content.ad.flag_delivery_failed.description', {
            count: adState.flagDeliveryFailures,
            defaultValue:
              '{{count}} service did not acknowledge this round’s flag. The operator has been notified; health evidence will identify affected services.',
          })}
        </Alert>
      )}

      {adState.flagsReady && service.currentFlag && (
        <Group gap={6} align="flex-start" wrap="nowrap">
          <Text size="xs" c="dimmed">
            {t('game.content.ad.flag_to_defend', 'Defending')}:
          </Text>
          <CopyButton value={service.currentFlag}>
            {({ copied, copy }) => (
              <Tooltip
                label={copied ? t('game.tooltip.copy.copied', 'Copied') : t('game.tooltip.copy.flag', 'Copy flag')}
              >
                <Text
                  className={misc.ffmono}
                  size="xs"
                  c="dimmed"
                  truncate
                  role="button"
                  tabIndex={0}
                  aria-label={t('game.tooltip.copy.flag', 'Copy flag')}
                  style={{ cursor: 'pointer' }}
                  onClick={copy}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' || e.key === ' ') {
                      e.preventDefault()
                      copy()
                    }
                  }}
                >
                  {service.currentFlag}
                </Text>
              </Tooltip>
            )}
          </CopyButton>
        </Group>
      )}

      {/* SSH-jump reaches the RSCTF-hosted container; for self-hosted (BYOC) there
          is none (the team's service is on their own machine), so hide the hint. */}
      {!service.selfHosted && renderSshHint()}

      {snapshotDownload}
    </Stack>
  )
}
