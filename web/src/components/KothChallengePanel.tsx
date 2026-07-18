import { Alert, Badge, CopyButton, Group, Loader, Stack, Text, Tooltip } from '@mantine/core'
import { mdiAlertCircleOutline, mdiCrown } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import useSWR from 'swr'
import { isKothResetTransition, kothConfirmationProgress, maxKothCooldownTicks } from '@Utils/kothLifecycle'
import { selectCurrentKothTarget } from '@Utils/kothTarget'
import type { KothLifecycleFields } from '@Hooks/useGame'
import api from '@Api'
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
// endpoints remain typed locally. Ad/Targets uses the exported SDK model below.
interface KothTokenModel {
  round: number
  token: string | null
  status: 'warmup' | 'no-cycle-token' | 'ready'
}

interface KothHillStateModel extends KothLifecycleFields {
  round: number
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
}

/**
 * Per-challenge King of the Hill status block. Mirrors the layout of
 * <see cref="AdChallengePanel"/> but for the shared hill model:
 *   - the hill IP:port (one shared container per challenge — copy-button so the
 *     player can drop it straight into curl);
 *   - the team's exact capability for this hill and crown cycle — copy-button
 *     to plant into /koth/king;
 *   - who's holding it right now (highlights when it's YOU);
 *   - the latest functional verdict on the hill.
 *
 * Uses useSWR with 5s polling so the holder + status update without manual
 * refresh — same cadence as the A&D panel's adState hook.
 */
export const KothChallengePanel: FC<KothChallengePanelProps> = ({ gameId, challengeId }) => {
  const { t } = useTranslation()

  // The Token endpoint requires player auth (cookie session). The token is
  // scoped to this hill and crown cycle. All three related views poll at the
  // same bounded cadence so a replacement address and capability converge
  // together without creating per-team database work.
  const { data: tokenData } = useSWR<KothTokenModel>(`/api/game/${gameId}/ad/koth/${challengeId}/token`, {
    refreshInterval: KOTH_POLL_INTERVAL_MS,
  })
  const { data: stateData } = useSWR<KothHillStateModel>(`/api/game/${gameId}/ad/koth/${challengeId}/state`, {
    refreshInterval: KOTH_POLL_INTERVAL_MS,
  })
  const { data: targets } = api.game.useGameAdTargets(gameId, { refreshInterval: KOTH_POLL_INTERVAL_MS })

  const resetPhase = stateData?.resetPhase ?? 'Active'
  const targetSnapshot = targets?.challenges.find((c) => c.challengeId === challengeId)?.hill
  const hill = selectCurrentKothTarget(
    targetSnapshot,
    stateData && { cycleNumber: stateData.cycleNumber, resetPhase: stateData.resetPhase }
  )
  // The state response binds its verdict to the same exact lifecycle/container
  // view as the holder. Use the Targets verdict only before state has loaded.
  const displayedStatus = stateData ? stateData.status : hill?.lastCheckStatus
  const isResetting = (stateData?.cycleNumber ?? 0) > 0 && isKothResetTransition(resetPhase)
  const [confirmationCurrent, confirmationRequired] = kothConfirmationProgress(
    stateData?.provisionalConfirmationTicks,
    stateData?.claimConfirmationTicks
  )
  const cooldown = stateData?.cooldownParticipants ?? []

  // Loading: neither came back yet → show a single spinner so the modal
  // doesn't flash empty.
  if (!tokenData && !stateData) {
    return (
      <Group justify="center" py="md">
        <Loader size="sm" />
      </Group>
    )
  }

  return (
    <Stack gap={6}>
      {/* Hill state — who holds it right now + functional verdict */}
      <Group justify="space-between" wrap="wrap" align="center">
        <Group gap="xs" wrap="nowrap">
          <Icon path={mdiCrown} size={0.7} color="var(--mantine-color-violet-6)" />
          <Text fw="bold" size="sm">
            {t('game.content.koth.hill', 'The hill')}
          </Text>
          <Badge size="sm" color={statusColor(displayedStatus)} variant={displayedStatus ? 'filled' : 'light'}>
            {displayedStatus ?? t('game.content.ad.no_checks_yet', 'no checks yet')}
          </Badge>
        </Group>
        {stateData?.holderTeamName && (
          <Badge size="sm" color={stateData.isYou ? 'violet' : 'gray'} variant={stateData.isYou ? 'filled' : 'light'}>
            {stateData.isYou
              ? t('game.content.koth.you_hold_it', 'You are the confirmed king')
              : t('game.content.koth.holder', {
                  team: stateData.holderTeamName,
                  defaultValue: 'Confirmed king: {{team}}',
                })}
          </Badge>
        )}
        {stateData?.provisionalClaimantTeamName && (
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

      {((stateData?.cycleNumber ?? 0) > 0 || stateData?.nextResetTicks != null || isResetting) && (
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
      )}

      {isResetting && (
        <Alert color={resetPhase === 'Failed' ? 'red' : 'orange'} variant="light" p="xs">
          <Text size="xs">
            {t(
              'game.content.koth.reset_pause',
              'The hill is being rebuilt and checked. Reset/readiness time is excluded from scoring.'
            )}
          </Text>
        </Alert>
      )}

      {cooldown.length > 0 && (
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
      {hill?.ip && (
        <Group gap={6} align="center" wrap="nowrap">
          <Text size="xs" c="dimmed">
            {t('game.content.ad.target', 'Target')}:
          </Text>
          <CopyButton value={`${hill.ip}:${hill.port ?? ''}`}>
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
                  {hill.ip}
                  {hill.port ? `:${hill.port}` : ''}
                </Text>
              </Tooltip>
            )}
          </CopyButton>
        </Group>
      )}

      {/* The team's cycle-scoped capability — copy-button so the player can plant it. */}
      <Group gap={6} align="center" wrap="nowrap">
        <Text size="xs" c="dimmed">
          {`${t('game.content.koth.your_token_short', 'Your cycle token')}:`}
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
            {t('game.content.koth.no_token', 'No capability was issued for this crown cycle')}
          </Text>
        )}
        {tokenData?.status === 'ready' && tokenData.token && (
          <CopyButton value={tokenData.token}>
            {({ copied, copy }) => (
              <Tooltip
                label={
                  copied
                    ? t('game.tooltip.copy.copied', 'Copied')
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
                  aria-label={t(
                    'game.tooltip.copy.koth_token',
                    'Copy this hill’s capability — write it into /koth/king on this hill'
                  )}
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
      </Group>

      {/* No hill rendered yet — operator hasn't ensured containers, or a crown-cycle
          reset is rebuilding it. Surface a hint instead of silence. */}
      {!hill?.ip && targets && stateData && (
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
        {t(
          'game.content.koth.patch_lifetime',
          'Patching is encouraged, but every patch and foothold lasts only until the next crown-cycle reset.'
        )}
      </Text>
    </Stack>
  )
}
