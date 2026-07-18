import { Modal } from '@mantine/core'
import { useMediaQuery, useReducedMotion } from '@mantine/hooks'
import { CSSProperties, FC, KeyboardEvent, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { FlagVerdictState } from '@Utils/FlagVerdict'
import classes from '@Styles/FlagVerdictOverlay.module.css'

interface FlagVerdictOverlayProps {
  verdict: FlagVerdictState
  challengeTitle: string
  score?: number
  onDismiss: () => void
}

type ParticleStyle = CSSProperties & Record<`--${string}`, string | number | undefined>

interface Particle {
  style: ParticleStyle
}

function createRandom(seedValue: number) {
  let seed = seedValue
  return () => {
    seed = (seed * 9301 + 49297) % 233280
    return seed / 233280
  }
}

function createParticles(kind: FlagVerdictState['kind'], count: number): Particle[] {
  const random = createRandom(kind === 'success' ? 2187 : 947)

  return Array.from({ length: count }, (_, index) => {
    const angle = random() * Math.PI * 2
    const distance = kind === 'success' ? 28 + random() * 40 : 16 + random() * 28
    const palette =
      kind === 'success' ? ['#5eead4', '#34d399', '#f8fafc', '#60a5fa', '#fbbf24'] : ['#fb7185', '#fda4af', '#fecdd3']

    if (kind === 'success') {
      return {
        style: {
          '--particle-color': palette[index % palette.length],
          '--particle-delay': `${420 + random() * 260}ms`,
          '--particle-duration': `${1150 + random() * 850}ms`,
          '--particle-rotation': `${Math.round(random() * 900 - 450)}deg`,
          '--particle-size': `${5 + random() * 8}px`,
          '--particle-x': `${Math.cos(angle) * distance}vmin`,
          '--particle-y': `${Math.sin(angle) * distance + 7}vmin`,
        },
      }
    }

    return {
      style: {
        '--particle-angle': `${angle}rad`,
        '--particle-delay': `${180 + random() * 180}ms`,
        '--particle-duration': `${520 + random() * 460}ms`,
        '--particle-length': `${18 + random() * 40}px`,
        '--particle-x': `${Math.cos(angle) * distance}vmin`,
        '--particle-y': `${Math.sin(angle) * distance}vmin`,
      },
    }
  })
}

export const FlagVerdictOverlay: FC<FlagVerdictOverlayProps> = ({ verdict, challengeTitle, score, onDismiss }) => {
  const { t, i18n } = useTranslation()
  const reducedMotion = useReducedMotion()
  const compact = useMediaQuery('(max-width: 38.75em)')
  const actionRef = useRef<HTMLButtonElement>(null)
  const [showParticles, setShowParticles] = useState(!reducedMotion)
  const success = verdict.kind === 'success'
  const particleCount = reducedMotion ? 0 : success ? (compact ? 24 : 48) : compact ? 12 : 18
  const particles = useMemo(() => createParticles(verdict.kind, particleCount), [particleCount, verdict.kind])

  const titleStart = t(
    success ? 'challenge.verdict.success.title_start' : 'challenge.verdict.wrong.title_start',
    success ? 'Challenge' : 'Signal'
  )
  const titleAccent = t(
    success ? 'challenge.verdict.success.title_accent' : 'challenge.verdict.wrong.title_accent',
    success ? 'Conquered' : 'Rejected'
  )
  const description = success
    ? t('challenge.verdict.success.description', '{{challenge}} solved. Nice work.', { challenge: challengeTitle })
    : t('challenge.verdict.wrong.description', 'No match. Check the flag and try again.')
  const actionLabel = success
    ? t('challenge.verdict.success.continue', 'Continue')
    : t('challenge.verdict.wrong.retry', 'Try again')

  useLayoutEffect(() => {
    actionRef.current?.focus({ preventScroll: true })
  }, [verdict.sequence])

  useEffect(() => {
    setShowParticles(!reducedMotion)
    if (reducedMotion) return

    const timer = window.setTimeout(() => setShowParticles(false), success ? 2900 : 1500)
    return () => window.clearTimeout(timer)
  }, [reducedMotion, success, verdict.sequence])

  const handleKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.key !== 'Escape') return
    event.preventDefault()
    event.stopPropagation()
    onDismiss()
  }

  const formattedScore = score === undefined ? null : new Intl.NumberFormat(i18n.language).format(score)

  return (
    <section className={classes.scene} data-kind={verdict.kind} onKeyDown={handleKeyDown}>
      <Modal.Title className={classes.srOnly}>{`${titleStart} ${titleAccent}`}</Modal.Title>

      <div className={classes.grid} aria-hidden="true" />
      <div className={classes.vignette} aria-hidden="true" />
      <div className={classes.scanlines} aria-hidden="true" />
      <div className={success ? classes.sweep : classes.wrongSweep} aria-hidden="true" />
      <div className={classes.orbit} aria-hidden="true" />
      <div className={`${classes.orbit} ${classes.orbitTwo}`} aria-hidden="true" />
      <div className={`${classes.orbit} ${classes.orbitThree}`} aria-hidden="true" />

      {showParticles && particleCount > 0 && (
        <div className={classes.particles} aria-hidden="true">
          {particles.map((particle, index) => (
            <i
              className={success ? classes.confetti : classes.shard}
              key={`${verdict.sequence}-${index}`}
              style={particle.style}
            />
          ))}
        </div>
      )}

      <button
        className={classes.closeButton}
        type="button"
        onClick={onDismiss}
        aria-label={t('challenge.verdict.close', 'Close result')}
      >
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="m6.4 5 5.6 5.6L17.6 5 19 6.4 13.4 12l5.6 5.6-1.4 1.4-5.6-5.6L6.4 19 5 17.6l5.6-5.6L5 6.4 6.4 5Z" />
        </svg>
      </button>

      <div className={classes.core}>
        <div className={classes.crest} aria-hidden="true">
          {success ? (
            <svg viewBox="0 0 32 32">
              <path d="m7 16.5 6 6L25 9" />
            </svg>
          ) : (
            <svg viewBox="0 0 32 32">
              <path d="M9 9l14 14" />
              <path d="M23 9 9 23" />
            </svg>
          )}
        </div>

        <h2 className={classes.title} aria-hidden="true">
          <span>{titleStart}</span>
          <em>{titleAccent}</em>
        </h2>

        <p className={classes.description}>{description}</p>

        {success && formattedScore !== null ? (
          <div className={classes.reward}>
            <span className={classes.rewardLabel}>{t('challenge.verdict.success.score', 'Score')}</span>
            <span className={classes.rewardLine} aria-hidden="true" />
            <strong>{formattedScore} PTS</strong>
          </div>
        ) : null}

        {!success && (
          <div className={classes.reward}>
            <span className={classes.rewardLabel}>{t('challenge.verdict.wrong.status', 'Status')}</span>
            <span className={classes.rewardLine} aria-hidden="true" />
            <strong>{t('challenge.verdict.wrong.result', 'Wrong flag')}</strong>
          </div>
        )}

        <button ref={actionRef} className={classes.actionButton} type="button" onClick={onDismiss} data-autofocus>
          {actionLabel}
        </button>
      </div>
    </section>
  )
}
