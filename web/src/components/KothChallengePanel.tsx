import { Alert, Badge, Button, CopyButton, Group, Loader, Stack, Text, Tooltip } from '@mantine/core'
import { useModals } from '@mantine/modals'
import { showNotification } from '@mantine/notifications'
import { mdiAlertCircleOutline, mdiApi, mdiCheck, mdiCrown, mdiRefresh } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useCallback, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import useSWR from 'swr'
import { assertJsonResponse } from '@Utils/ChallengePolling'
import {
  claimPlayerCredentialOperation,
  clearPlayerCredentialOperation,
  ownsPlayerCredentialResult,
  playerCredentialOperationStorageKey,
  readPlayerCredentialOperation,
} from '@Utils/PlayerCredentialOperations'
import { showErrorMsg } from '@Utils/Shared'
import { isKothResetTransition, kothConfirmationProgress, maxKothCooldownTicks } from '@Utils/kothLifecycle'
import { CompletionPollSWRConfig, jitterPollingDelay, useCompletionPolling } from '@Hooks/useCompletionPolling'
import type { KothLifecycleFields } from '@Hooks/useGame'
import api, { ContentType } from '@Api'
import misc from '@Styles/Misc.module.css'

const KOTH_POLL_INTERVAL_MS = 5_000

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

// These KotH-only shapes are not in the generated SDK yet, so the two direct
// endpoints remain typed locally.
interface KothTokenModel {
  round: number
  token: string | null
  status: 'warmup' | 'no-cycle-token' | 'ready'
  revision: number
  operationId?: string | null
}

interface KothHillStateModel extends KothLifecycleFields {
  round: number
  ip: string | null
  port: number | null
  claimSource: 'Api' | 'Marker' | string
  holderParticipationId: number | null
  holderTeamName: string | null
  isYou: boolean
  claimConfirmationTicks: number
  cycleTicks: number
  eligibleNow: boolean
  isYouCooldown: boolean
  status: string | null
  checkedAt: number | null
}

interface KothChallengePanelProps {
  gameId: number
  challengeId: number
  active: boolean
}

/** Abort the one request owned by a modal key when that modal closes. */
const useAbortableApiRead = <T,>(enabled: boolean) => {
  const pending = useRef<AbortController | null>(null)

  useEffect(() => {
    if (!enabled) {
      pending.current?.abort()
      pending.current = null
    }
    return () => {
      pending.current?.abort()
      pending.current = null
    }
  }, [enabled])

  return useCallback(async (path: string) => {
    pending.current?.abort()
    const controller = new AbortController()
    pending.current = controller
    try {
      const response = await api.request<T>({ path, method: 'GET', format: 'json', signal: controller.signal })
      return assertJsonResponse(response)
    } finally {
      if (pending.current === controller) pending.current = null
    }
  }, [])
}

/**
 * Per-challenge King of the Hill status block. Mirrors the layout of
 * <see cref="AdChallengePanel"/> but for the shared hill model:
 *   - the hill IP:port (one shared container per challenge — copy-button so the
 *     player can drop it straight into curl);
 *   - the team's exact cycle capability or event-stable arena capability;
 *   - marker-holder state, or the Leaderboard play model;
 *   - the latest functional verdict on the hill.
 *
 * Each key uses a completion-scheduled five-second cadence so the holder and
 * status update without overlapping slow requests or SWR's error retries.
 */
export const KothChallengePanel: FC<KothChallengePanelProps> = ({ gameId, challengeId, active }) => {
  const { t } = useTranslation()
  const modals = useModals()
  const [rotating, setRotating] = useState(false)

  // The Token endpoint requires player auth (cookie session). The token is
  // scoped to this hill. Marker tokens rotate per crown cycle; Leaderboard
  // capabilities remain stable for the event unless the player rotates one.
  const enabled = active && gameId > 0 && challengeId > 0
  const tokenKey = enabled ? `/api/game/${gameId}/ad/koth/${challengeId}/token` : null
  const stateKey = enabled ? `/api/game/${gameId}/ad/koth/${challengeId}/state` : null
  const tokenFetcher = useAbortableApiRead<KothTokenModel>(enabled)
  const stateFetcher = useAbortableApiRead<KothHillStateModel>(enabled)
  const {
    data: tokenData,
    error: tokenError,
    isValidating: tokenValidating,
    mutate: mutateToken,
  } = useSWR<KothTokenModel>(tokenKey, tokenFetcher, CompletionPollSWRConfig)
  const {
    data: stateData,
    error: stateError,
    isValidating: stateValidating,
    mutate: mutateState,
  } = useSWR<KothHillStateModel>(stateKey, stateFetcher, CompletionPollSWRConfig)
  useCompletionPolling({
    key: tokenKey ?? '',
    phase: 'open',
    enabled,
    data: tokenData,
    error: tokenError,
    isValidating: tokenValidating,
    mutate: mutateToken,
    successDelay: () => jitterPollingDelay(KOTH_POLL_INTERVAL_MS),
  })
  useCompletionPolling({
    key: stateKey ?? '',
    phase: 'open',
    enabled,
    data: stateData,
    error: stateError,
    isValidating: stateValidating,
    mutate: mutateState,
    successDelay: () => jitterPollingDelay(KOTH_POLL_INTERVAL_MS),
  })

  const resetPhase = stateData?.resetPhase ?? 'Active'
  const displayedStatus = stateData?.status
  const isResetting = (stateData?.cycleNumber ?? 0) > 0 && isKothResetTransition(resetPhase)
  const [confirmationCurrent, confirmationRequired] = kothConfirmationProgress(
    stateData?.provisionalConfirmationTicks,
    stateData?.claimConfirmationTicks
  )
  const cooldown = stateData?.cooldownParticipants ?? []
  const isApiArena = stateData?.claimSource === 'Api'

  const confirmRotation = () => {
    modals.openConfirmModal({
      title: t('game.content.koth.rotate_capability_title', 'Rotate arena capability?'),
      children: (
        <Text size="sm">
          {t(
            'game.content.koth.rotate_capability_warning',
            'The old token stops scoring immediately. Your settled RSCTF points remain, but you must reconnect to the arena with the new token.'
          )}
        </Text>
      ),
      labels: {
        confirm: t('game.button.koth.rotate_capability', 'Rotate token'),
        cancel: t('common.cancel', 'Cancel'),
      },
      confirmProps: { color: 'orange' },
      onConfirm: async () => {
        setRotating(true)
        try {
          const operationKey = playerCredentialOperationStorageKey(gameId, 'koth-api', challengeId)
          const claim = () =>
            claimPlayerCredentialOperation(window.localStorage, operationKey, tokenData?.revision ?? 0)
          const operation =
            typeof navigator !== 'undefined' && navigator.locks
              ? await navigator.locks.request(`rsctf:${operationKey}`, claim)
              : claim()
          const response = await api.request<KothTokenModel>({
            path: `/api/game/${gameId}/ad/koth/${challengeId}/token`,
            method: 'POST',
            body: {
              operationId: operation.operationId,
              expectedRevision: operation.expectedRevision,
            },
            type: ContentType.Json,
            format: 'json',
          })
          if (!ownsPlayerCredentialResult(window.localStorage, operationKey, operation, response.data)) {
            throw new Error('A stale KotH credential response was ignored')
          }
          clearPlayerCredentialOperation(window.localStorage, operationKey, operation.operationId)
          await mutateToken(response.data, { revalidate: false })
          showNotification({
            color: 'teal',
            icon: <Icon path={mdiCheck} size={1} />,
            message: t(
              'game.notification.koth.capability_rotated',
              'Arena capability rotated. Reconnect with the new token.'
            ),
          })
        } catch (error) {
          if ((error as { response?: { status?: number } })?.response?.status === 409) {
            const operationKey = playerCredentialOperationStorageKey(gameId, 'koth-api', challengeId)
            const operationId = readPlayerCredentialOperation(window.localStorage, operationKey)?.operationId
            if (operationId) {
              clearPlayerCredentialOperation(window.localStorage, operationKey, operationId)
            }
          }
          await mutateToken().catch(() => undefined)
          showErrorMsg(error, t)
        } finally {
          setRotating(false)
        }
      },
    })
  }

  // Loading: neither came back yet → show a single spinner so the modal
  // doesn't flash empty.
  if (!tokenData && !stateData) {
    if (tokenError || stateError) {
      return (
        <Alert icon={<Icon path={mdiAlertCircleOutline} size={0.9} />} color="red" variant="light">
          <Stack gap="xs">
            <Text size="sm">
              {t('game.content.koth.live_load_error', 'The live hill information could not be loaded.')}
            </Text>
            <Button
              size="compact-xs"
              variant="light"
              leftSection={<Icon path={mdiRefresh} size={0.7} />}
              onClick={() => {
                void mutateToken()
                void mutateState()
              }}
            >
              {t('common.button.retry', 'Retry')}
            </Button>
          </Stack>
        </Alert>
      )
    }
    return (
      <Group justify="center" py="md">
        <Loader size="sm" />
      </Group>
    )
  }

  return (
    <Stack gap={6}>
      {(tokenError || stateError) && (
        <Alert icon={<Icon path={mdiAlertCircleOutline} size={0.9} />} color="orange" variant="light" p="xs">
          <Group justify="space-between" gap="xs" wrap="wrap">
            <Text size="xs">
              {t('game.content.koth.live_partial_error', 'Some live hill information could not be refreshed.')}
            </Text>
            <Button
              size="compact-xs"
              variant="subtle"
              leftSection={<Icon path={mdiRefresh} size={0.65} />}
              onClick={() => {
                if (tokenError) void mutateToken()
                if (stateError) void mutateState()
              }}
            >
              {t('common.button.retry', 'Retry')}
            </Button>
          </Group>
        </Alert>
      )}
      {/* Hill state — who holds it right now + functional verdict */}
      <Group justify="space-between" wrap="wrap" align="center">
        <Group gap="xs" wrap="nowrap">
          <Icon
            path={isApiArena ? mdiApi : mdiCrown}
            size={0.7}
            color={isApiArena ? 'var(--mantine-color-blue-6)' : 'var(--mantine-color-violet-6)'}
          />
          <Text fw="bold" size="sm">
            {isApiArena ? t('game.content.koth.api_arena', 'Leaderboard') : t('game.content.koth.hill', 'The hill')}
          </Text>
          <Badge size="sm" color={statusColor(displayedStatus)} variant={displayedStatus ? 'filled' : 'light'}>
            {displayedStatus ?? t('game.content.ad.no_checks_yet', 'no checks yet')}
          </Badge>
        </Group>
        {!isApiArena && stateData?.holderTeamName && (
          <Badge size="sm" color={stateData.isYou ? 'violet' : 'gray'} variant={stateData.isYou ? 'filled' : 'light'}>
            {stateData.isYou
              ? t('game.content.koth.you_hold_it', 'You are the confirmed king')
              : t('game.content.koth.holder', {
                  team: stateData.holderTeamName,
                  defaultValue: 'Confirmed king: {{team}}',
                })}
          </Badge>
        )}
        {!isApiArena && stateData?.provisionalClaimantTeamName && (
          <Badge size="sm" color="orange" variant="light">
            {t('game.content.koth.provisional_holder', {
              team: stateData.provisionalClaimantTeamName,
              current: confirmationCurrent,
              required: confirmationRequired,
              defaultValue: 'Provisional: {{team}} · {{current}}/{{required}}',
            })}
          </Badge>
        )}
      </Group>

      {isApiArena && (
        <Alert color="blue" variant="light" p="xs">
          <Text size="xs">
            {t(
              'game.content.koth.api_play',
              'Every team can score in each challenge-native wave. Complete a fresh run through the challenge’s documented actions; RSCTF awards 95% from performance relative to that wave’s best result and 5% for its first-place Crown. Failed hacking attempts are not negative points, and an absent team receives zero.'
            )}
          </Text>
        </Alert>
      )}

      {isApiArena ? (
        <Group gap={6} wrap="wrap">
          <Badge size="xs" color="blue" variant="light">
            {t('game.content.koth.api_persistent_arena', 'Persistent arena · health supervised')}
          </Badge>
          {isResetting && (
            <Badge size="xs" color={resetPhase === 'Failed' ? 'red' : 'orange'} variant="filled">
              {t('game.content.koth.api_recovery_phase', {
                phase: resetPhase,
                defaultValue: 'Health recovery: {{phase}}',
              })}
            </Badge>
          )}
        </Group>
      ) : (stateData?.cycleNumber ?? 0) > 0 || stateData?.nextResetTicks != null || isResetting ? (
        <Group gap={6} wrap="wrap">
          {(stateData?.cycleNumber ?? 0) > 0 && (
            <Badge size="xs" color="violet" variant="light">
              {t('game.content.koth.cycle_number', {
                cycle: stateData?.cycleNumber ?? 0,
                defaultValue: 'Cycle {{cycle}}',
              })}
            </Badge>
          )}
          {(stateData?.cycleNumber ?? 0) > 0 && stateData?.cycleTick != null && stateData?.cycleTicks != null && (
            <Badge size="xs" color="blue" variant="light">
              {t('game.content.koth.cycle_tick', {
                tick: stateData.cycleTick,
                total: stateData.cycleTicks,
                defaultValue: 'Tick {{tick}}/{{total}}',
              })}
            </Badge>
          )}
          {isResetting ? (
            <Badge size="xs" color={resetPhase === 'Failed' ? 'red' : 'orange'} variant="filled">
              {t('game.content.koth.reset_phase', {
                phase: resetPhase,
                defaultValue: 'Reset: {{phase}}',
              })}
            </Badge>
          ) : stateData?.nextResetTicks != null ? (
            <Badge size="xs" color="gray" variant="light">
              {t('game.content.koth.next_reset', {
                count: stateData.nextResetTicks,
                defaultValue: 'Reset in {{count}} tick(s)',
              })}
            </Badge>
          ) : null}
        </Group>
      ) : null}

      {isResetting && (
        <Alert color={resetPhase === 'Failed' ? 'red' : 'orange'} variant="light" p="xs">
          <Text size="xs">
            {t(
              'game.content.koth.reset_pause',
              isApiArena
                ? 'The arena failed health supervision and is being rebuilt. Recovery time is excluded from scoring; your event token remains valid.'
                : 'The hill is being rebuilt and checked. Reset/readiness time is excluded from scoring.'
            )}
          </Text>
        </Alert>
      )}

      {!isApiArena && cooldown.length > 0 && (
        <Alert color={stateData?.isYouCooldown ? 'orange' : 'violet'} variant="light" p="xs">
          <Text size="xs">
            {stateData?.isYouCooldown
              ? t('game.content.koth.cooldown_you', {
                  count: maxKothCooldownTicks(cooldown),
                  defaultValue:
                    'Champion cooldown: your team cannot reach or claim this hill for {{count}} more tick(s). The tick is removed from your eligible denominator.',
                })
              : t('game.content.koth.cooldown_teams', {
                  teams: cooldown.map((entry) => entry.teamName).join(', '),
                  count: maxKothCooldownTicks(cooldown),
                  defaultValue: 'Champion cooldown: {{teams}} · {{count}} tick(s) remaining.',
                })}
          </Text>
        </Alert>
      )}

      {/* Hill IP:port — copy-button to drop into curl */}
      {stateData?.ip && (
        <Group gap={6} align="center" wrap="nowrap">
          <Text size="xs" c="dimmed">
            {t('game.content.ad.target', 'Target')}:
          </Text>
          <CopyButton value={`${stateData.ip}:${stateData.port ?? ''}`}>
            {({ copied, copy }) => (
              <Tooltip
                label={
                  copied ? t('game.tooltip.copy.copied', 'Copied') : t('game.tooltip.copy.target', 'Copy hill address')
                }
              >
                <Text
                  className={misc.ffmono}
                  size="xs"
                  truncate
                  role="button"
                  tabIndex={0}
                  aria-label={t('game.tooltip.copy.target', 'Copy hill address')}
                  style={{ cursor: 'pointer' }}
                  onClick={copy}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' || e.key === ' ') {
                      e.preventDefault()
                      copy()
                    }
                  }}
                >
                  {stateData.ip}
                  {stateData.port ? `:${stateData.port}` : ''}
                </Text>
              </Tooltip>
            )}
          </CopyButton>
        </Group>
      )}

      {/* Marker hills plant a cycle capability. A Leaderboard challenge consumes
          its event-stable capability only through challenge-defined actions. */}
      <Group gap={6} align="center" wrap="nowrap">
        <Text size="xs" c="dimmed">
          {`${
            isApiArena
              ? t('game.content.koth.your_arena_capability', 'Your arena capability')
              : t('game.content.koth.your_token_short', 'Your cycle token')
          }:`}
        </Text>
        {/* No data yet (initial load or a failed token fetch) — show a hint rather
            than a bare label with a blank value, which looks broken. */}
        {!tokenData && (
          <Text size="xs" c="dimmed" fs="italic">
            {t('game.content.koth.token_loading', 'loading…')}
          </Text>
        )}
        {tokenData?.status === 'warmup' && (
          <Text size="xs" c="dimmed" fs="italic">
            {t('game.content.koth.warmup', 'Game hasn’t started ticking yet')}
          </Text>
        )}
        {tokenData?.status === 'no-cycle-token' && (
          <Text size="xs" c="orange" fs="italic">
            {isApiArena
              ? t('game.content.koth.no_api_token', 'No arena capability has been issued yet')
              : t('game.content.koth.no_token', 'No capability was issued for this crown cycle')}
          </Text>
        )}
        {tokenData?.status === 'ready' && tokenData.token && (
          <CopyButton value={tokenData.token}>
            {({ copied, copy }) => (
              <Tooltip
                label={
                  copied
                    ? t('game.tooltip.copy.copied', 'Copied')
                    : isApiArena
                      ? t(
                          'game.tooltip.copy.koth_api_token',
                          'Copy this capability — paste it as the arena’s only login value'
                        )
                      : t(
                          'game.tooltip.copy.koth_token',
                          'Copy this hill’s capability — write it into /koth/king on this hill'
                        )
                }
              >
                <Text
                  className={misc.ffmono}
                  size="xs"
                  fw="bold"
                  truncate
                  role="button"
                  tabIndex={0}
                  aria-label={
                    isApiArena
                      ? t(
                          'game.tooltip.copy.koth_api_token',
                          'Copy this capability — paste it as the arena’s only login value'
                        )
                      : t(
                          'game.tooltip.copy.koth_token',
                          'Copy this hill’s capability — write it into /koth/king on this hill'
                        )
                  }
                  style={{ cursor: 'pointer', maxWidth: 320 }}
                  onClick={copy}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' || e.key === ' ') {
                      e.preventDefault()
                      copy()
                    }
                  }}
                >
                  {tokenData.token}
                </Text>
              </Tooltip>
            )}
          </CopyButton>
        )}
        {isApiArena && tokenData?.status === 'ready' && (
          <Button
            size="compact-xs"
            color="orange"
            variant="subtle"
            loading={rotating}
            leftSection={<Icon path={mdiRefresh} size={0.7} />}
            onClick={confirmRotation}
          >
            {t('game.button.koth.rotate_capability', 'Rotate token')}
          </Button>
        )}
      </Group>

      {/* No hill rendered yet — the operator has not ensured containers, or a
          lifecycle transition is rebuilding it. Surface a hint instead of silence. */}
      {!stateData?.ip && stateData && (
        <Alert icon={<Icon path={mdiAlertCircleOutline} size={0.9} />} color="orange" variant="light" p="xs">
          <Text size="xs">
            {t(
              'game.content.koth.no_hill',
              'Hill not running yet. If this persists, ask the operator to ensure containers.'
            )}
          </Text>
        </Alert>
      )}

      <Text size="xs" c="dimmed">
        {isApiArena
          ? t(
              'game.content.koth.api_capability_lifetime',
              'Use only this token to enter the arena; RSCTF supplies your team identity automatically. It stays valid for the entire event unless you rotate it after exposure.'
            )
          : t(
              'game.content.koth.patch_lifetime',
              'Patching is encouraged, but every patch and foothold lasts only until the next crown-cycle reset.'
            )}
      </Text>
    </Stack>
  )
}
