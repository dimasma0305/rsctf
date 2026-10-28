import { Button, Group } from '@mantine/core'
import { mdiCrown, mdiSwordCross } from '@mdi/js'
import { Icon } from '@mdi/react'
import { useTranslation } from 'react-i18next'
import { adRoundSecondsRemaining } from '@Utils/adState'
import { epochProgress } from '@Utils/epochProgress'
import type { AdStateModel } from '@Api'
import { CompetitionNotice } from './CompetitionNotice'
import classes from './CompetitionStatus.module.css'
import { FlagDeliveryNotice } from './FlagDeliveryNotice'

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
          {
            title: t('game.content.ad.scoring_paused', 'Scoring paused'),
            description: t(
              'game.content.ad.scoring_paused_description',
              'Round progression, checker scoring, and captured-flag submissions are paused by the event operator.'
            ),
          },
        ]
      : []),
    ...(state?.currentRound === 0
      ? [
          {
            title: t('game.content.ad.warmup_pill', 'Warmup — scoring not yet active'),
            description: t(
              'game.arena.warmup_help',
              'The first scoring round has not started. Use the toolkit to prepare your connection and wait for the organizer to start the event.'
            ),
          },
        ]
      : []),
    ...(hasAd && state && state.currentRound > 0 && !state.flagsReady
      ? [
          {
            title: t('game.content.ad.flags_syncing.label', 'Flags syncing — wait before attacking'),
            description: t(
              'game.content.ad.flags_syncing.description',
              'This round’s flags are still syncing. Wait before attacking or updating your defended flag.'
            ),
          },
        ]
      : []),
    ...(awaitingUpdate
      ? [
          {
            title: t('game.arena.awaiting_round_update', 'Awaiting round update'),
            description: t(
              'game.arena.awaiting_round_help',
              'The last reported round deadline has passed, but a newer round has not arrived. The page refreshes automatically. If this persists, check your connection and ask the organizer to inspect the round engine; a zero timer does not prove a new round has started.'
            ),
          },
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
        {(messages.length > 0 || (hasAd && state && state.currentRound > 0 && state.flagDeliveryFailures > 0)) && (
          <div className={classes.messages} data-round-messages>
            {messages.map(({ title, description }) => (
              <CompetitionNotice key={title} title={title}>
                <p>{description}</p>
              </CompetitionNotice>
            ))}
            {hasAd && state && (
              <FlagDeliveryNotice
                count={state.flagDeliveryFailures}
                round={state.currentRound}
                onToolkit={onAdToolkit}
              />
            )}
          </div>
        )}
      </div>
    </section>
  )
}
