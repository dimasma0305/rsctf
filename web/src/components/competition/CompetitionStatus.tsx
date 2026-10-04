import { Button, Group, Text } from '@mantine/core'
import { mdiAlertCircleOutline, mdiCrown, mdiPauseCircleOutline, mdiSwordCross } from '@mdi/js'
import { Icon } from '@mdi/react'
import { useTranslation } from 'react-i18next'
import { adRoundSecondsRemaining } from '@Utils/adState'
import { epochProgress } from '@Utils/epochProgress'
import type { AdStateModel } from '@Api'
import classes from './CompetitionStatus.module.css'

interface CompetitionStatusProps {
  state?: AdStateModel
  nowMs: number
  hasAd: boolean
  hasKoth: boolean
  onAdToolkit: () => void
  onKothToolkit: () => void
}

// Presentation only: the route remains the sole owner of the existing state read.
export const CompetitionStatus = ({
  state,
  nowMs,
  hasAd,
  hasKoth,
  onAdToolkit,
  onKothToolkit,
}: CompetitionStatusProps) => {
  const { t } = useTranslation()
  const seconds = adRoundSecondsRemaining(state?.roundEndsAt, nowMs, !!state?.scoringPaused, state?.scoringPausedAt)
  const epoch = state && hasAd ? epochProgress(state.currentRound, state.startRound, state.epochTicks) : null
  const awaitingUpdate = !!state && state.currentRound > 0 && !state.scoringPaused && seconds === 0
  const messages = [
    ...(state?.scoringPaused
      ? [
          t(
            'game.content.ad.scoring_paused_description',
            'Round progression, checker scoring, and captured-flag submissions are paused by the event operator.'
          ),
        ]
      : []),
    ...(state?.currentRound === 0 ? [t('game.content.ad.warmup_pill', 'Warmup — scoring not yet active')] : []),
    ...(hasAd && state && state.currentRound > 0 && !state.flagsReady
      ? [t('game.content.ad.flags_syncing.label', 'Flags syncing — wait before attacking')]
      : []),
    ...(hasAd && state && state.flagDeliveryFailures > 0
      ? [
          t('game.content.ad.flag_delivery_failed.label', {
            count: state.flagDeliveryFailures,
            defaultValue: '{{count}} flag deliveries need attention',
          }),
        ]
      : []),
  ]

  return (
    <section
      className={classes.status}
      aria-label={t('game.arena.scoring_status', 'Scoring status')}
      data-competition-status
    >
      <div className={classes.row}>
        <dl className={classes.metrics}>
          <div>
            <dt>{t('game.content.ad.round', 'Round')}</dt>
            <dd>{state?.currentRound ?? '—'}</dd>
          </div>
          <div className={classes.deadline}>
            <dt>
              {state?.scoringPaused
                ? t('game.content.ad.scoring_paused', 'Scoring paused')
                : awaitingUpdate
                  ? t('game.arena.status', 'Status')
                  : t('game.content.ad.round_ends', 'Round ends')}
            </dt>
            <dd>
              <span data-round-deadline role="timer" aria-live="off">
                {awaitingUpdate
                  ? t('game.arena.awaiting_round_update', 'Awaiting round update')
                  : seconds === null
                    ? '—'
                    : `${seconds}s`}
              </span>
            </dd>
          </div>
          {epoch && (
            <>
              <div>
                <dt>{t('game.arena.epoch', 'Epoch')}</dt>
                <dd>{epoch.epoch}</dd>
              </div>
              <div>
                <dt>{t('game.arena.tick', 'Tick')}</dt>
                <dd>
                  {epoch.tick}/{epoch.totalTicks}
                </dd>
              </div>
            </>
          )}
        </dl>
        <Group gap="xs" className={classes.tools}>
          {hasAd && (
            <Button
              variant="default"
              size="xs"
              leftSection={<Icon path={mdiSwordCross} size={0.8} aria-hidden="true" />}
              onClick={onAdToolkit}
            >
              {t('game.button.ad.open_toolkit', 'A&D Toolkit')}
            </Button>
          )}
          {hasKoth && (
            <Button
              variant="default"
              size="xs"
              leftSection={<Icon path={mdiCrown} size={0.8} aria-hidden="true" />}
              onClick={onKothToolkit}
            >
              {t('game.button.koth.open_toolkit', 'KotH Toolkit')}
            </Button>
          )}
        </Group>
      </div>
      <div role="status" aria-live="polite" aria-atomic="true">
        {messages.length > 0 && (
          <div className={classes.messages} data-round-messages>
            <Icon
              path={state?.scoringPaused ? mdiPauseCircleOutline : mdiAlertCircleOutline}
              size={0.85}
              aria-hidden="true"
            />
            <div>
              {messages.map((message) => (
                <Text key={message} size="xs">
                  {message}
                </Text>
              ))}
            </div>
          </div>
        )}
      </div>
    </section>
  )
}
