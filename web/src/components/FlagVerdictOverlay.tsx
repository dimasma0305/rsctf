import { Modal } from '@mantine/core'
import { CSSProperties, FC, KeyboardEvent, useEffect, useLayoutEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { FlagVerdictState } from '@Utils/FlagVerdict'
import classes from '@Styles/FlagVerdictOverlay.module.css'

interface FlagVerdictOverlayProps {
  verdict: FlagVerdictState
  challengeTitle: string
  score?: number
  onDismiss: () => void
}

export const FlagVerdictDialog: FC<Omit<FlagVerdictOverlayProps, 'verdict'> & { verdict: FlagVerdictState | null }> = ({
  verdict,
  ...props
}) => {
  // Retain one result through the exit transition; removing its children on
  // dismissal makes the dialog collapse before it has finished fading out.
  const [lastVerdict, setLastVerdict] = useState(verdict)
  useEffect(() => {
    if (verdict) setLastVerdict(verdict)
  }, [verdict])
  const displayedVerdict = verdict ?? lastVerdict
  return (
    <Modal.Root
      opened={!!verdict}
      onClose={props.onDismiss}
      closeOnEscape={false}
      closeOnClickOutside={false}
      returnFocus={false}
      // The app already reserves a stable scrollbar gutter. A second gap
      // compensation shifts the cards when the accepted-review form appears.
      removeScrollProps={{ removeScrollBar: false }}
      centered
      zIndex={6000}
      size="min(36rem, calc(100vw - 1.5rem))"
      transitionProps={{
        transition: {
          in: { opacity: 1, transform: 'translateY(0)' },
          out: { opacity: 0, transform: 'translateY(8px)' },
          common: {},
          transitionProperty: 'opacity, transform',
        },
        duration: 180,
        exitDuration: 140,
        timingFunction: 'cubic-bezier(0.2, 0, 0, 1)',
      }}
    >
      <Modal.Overlay backgroundOpacity={0.35} />
      <Modal.Content className={classes.dialog} inert={!verdict}>
        {displayedVerdict && (
          <FlagVerdictOverlay key={displayedVerdict.sequence} verdict={displayedVerdict} {...props} />
        )}
      </Modal.Content>
    </Modal.Root>
  )
}

export const FlagVerdictOverlay: FC<FlagVerdictOverlayProps> = ({ verdict, challengeTitle, score, onDismiss }) => {
  const { t, i18n } = useTranslation()
  const actionRef = useRef<HTMLButtonElement>(null)
  const success = verdict.kind === 'success'
  const titleStart = t(
    success ? 'challenge.verdict.success.title_start' : 'challenge.verdict.wrong.title_start',
    'Flag'
  )
  const titleAccent = t(
    success ? 'challenge.verdict.success.title_accent' : 'challenge.verdict.wrong.title_accent',
    success ? 'Accepted' : 'Denied'
  )
  const description = success
    ? t('challenge.verdict.success.description', 'Flag verified. Nicely done.', { challenge: challengeTitle })
    : t('challenge.verdict.wrong.description', 'That flag did not match. Check your answer and try again.')
  const actionLabel = success
    ? t('challenge.verdict.success.continue', 'Back to challenge')
    : t('challenge.verdict.wrong.retry', 'Try again')
  const formattedScore =
    score !== undefined && Number.isFinite(score) ? new Intl.NumberFormat(i18n.language).format(score) : null

  useLayoutEffect(() => {
    actionRef.current?.focus({ preventScroll: true })
  }, [verdict.sequence])

  const handleKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.key !== 'Escape') return
    event.preventDefault()
    event.stopPropagation()
    onDismiss()
  }

  return (
    <section className={classes.scene} data-kind={verdict.kind} data-flag-verdict onKeyDown={handleKeyDown}>
      <div className={classes.header}>
        <span className={classes.eyebrow}>{t('challenge.verdict.label', 'Submission result')}</span>
        <button
          className={classes.closeButton}
          type="button"
          onClick={onDismiss}
          aria-label={t('challenge.verdict.close', 'Close result')}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="m7 7 10 10M17 7 7 17" />
          </svg>
        </button>
      </div>

      <div className={classes.core}>
        <div className={classes.emblem} aria-hidden="true">
          <div className={classes.halo} />
          {/* One short CSS burst; no animation loop, timers, or random layout. */}
          {success && (
            <div className={classes.sparks}>
              {Array.from({ length: 12 }, (_, index) => (
                <i key={index} style={{ '--spark-angle': `${index * 30}deg` } as CSSProperties} />
              ))}
            </div>
          )}
          <svg className={classes.mark} viewBox="0 0 120 120">
            <circle className={classes.track} cx="60" cy="60" r="49" />
            <circle className={classes.ring} cx="60" cy="60" r="49" pathLength="1" />
            <circle className={classes.disc} cx="60" cy="60" r="39" />
            {success ? (
              <path className={classes.glyph} pathLength="1" d="m41 61 13 13 26-29" />
            ) : (
              <path className={classes.glyph} pathLength="1" d="m46 46 28 28m0-28L46 74" />
            )}
          </svg>
        </div>

        <Modal.Title className={classes.title}>
          {titleStart} <span>{titleAccent}</span>
        </Modal.Title>
        <p className={classes.challenge}>{challengeTitle}</p>
        <p className={classes.description}>{description}</p>

        {success && formattedScore !== null && (
          <div className={classes.reward}>
            <span>{t('challenge.verdict.success.value', 'Challenge value')}</span>
            <strong>
              {formattedScore} <span>{t('challenge.verdict.points', 'pts')}</span>
            </strong>
          </div>
        )}

        <button ref={actionRef} className={classes.actionButton} type="button" onClick={onDismiss} data-autofocus>
          {actionLabel}
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M5 12h14m-6-6 6 6-6 6" />
          </svg>
        </button>
      </div>
    </section>
  )
}
