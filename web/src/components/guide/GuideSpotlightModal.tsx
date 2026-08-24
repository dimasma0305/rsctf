import { Modal } from '@mantine/core'
import { mdiCursorDefaultClickOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import { CSSProperties, FC, PropsWithChildren, useEffect, useLayoutEffect, useRef, useState } from 'react'
import classes from '@Styles/PlayerGuide.module.css'

interface GuideTargetRect {
  left: number
  top: number
  right: number
  bottom: number
  width: number
  height: number
  viewportWidth: number
  viewportHeight: number
}

interface GuideSpotlightModalProps extends PropsWithChildren {
  opened: boolean
  onClose: () => void
  title: string
  closeLabel: string
  size: string
  overlayOpacity: number
  targetSelector?: string
}

const sameRect = (left: GuideTargetRect | null, right: GuideTargetRect | null) => {
  if (!left || !right) return left === right
  return (
    Math.abs(left.left - right.left) < 0.5 &&
    Math.abs(left.top - right.top) < 0.5 &&
    Math.abs(left.width - right.width) < 0.5 &&
    Math.abs(left.height - right.height) < 0.5 &&
    left.viewportWidth === right.viewportWidth &&
    left.viewportHeight === right.viewportHeight
  )
}

const renderedTargets = (selector?: string) => {
  if (!selector) return null
  const elements = selector
    .split(',')
    .map((candidate) => candidate.trim())
    .filter(Boolean)
    .flatMap((candidate) => Array.from(document.querySelectorAll<HTMLElement>(candidate)))
  return [...new Set(elements)].filter((element) => {
    const rect = element.getBoundingClientRect()
    const style = window.getComputedStyle(element)
    return rect.width > 0 && rect.height > 0 && style.display !== 'none' && style.visibility !== 'hidden'
  })
}

const targetVisibleRatio = (element: HTMLElement) => {
  const rect = element.getBoundingClientRect()
  const visibleWidth = Math.max(0, Math.min(rect.right, window.innerWidth) - Math.max(rect.left, 0))
  const visibleHeight = Math.max(0, Math.min(rect.bottom, window.innerHeight) - Math.max(rect.top, 0))
  return rect.width > 0 && rect.height > 0 ? (visibleWidth * visibleHeight) / (rect.width * rect.height) : 0
}

const targetCenterIsUsable = (element: HTMLElement) => {
  const rect = element.getBoundingClientRect()
  const left = Math.max(0, rect.left)
  const right = Math.min(window.innerWidth, rect.right)
  const top = Math.max(0, rect.top)
  const bottom = Math.min(window.innerHeight, rect.bottom)
  if (right <= left || bottom <= top) return false

  const elements = document.elementsFromPoint((left + right) / 2, (top + bottom) / 2)
  const topPageElement = elements.find(
    (candidate) => !candidate.closest('[data-guide-surface="coachmark"]') && !candidate.closest('[data-guide-layer]')
  )
  return Boolean(topPageElement && (topPageElement === element || element.contains(topPageElement)))
}

const isUsableTarget = (element: HTMLElement) => targetVisibleRatio(element) >= 0.6 && targetCenterIsUsable(element)

const visibleTarget = (selector?: string) => {
  const preferred = renderedTargets(selector)?.[0]
  return preferred && isUsableTarget(preferred) ? preferred : null
}

const measureTarget = (selector?: string): GuideTargetRect | null => {
  const element = visibleTarget(selector)
  if (!element) return null

  const measured = element.getBoundingClientRect()
  const padding = 8
  const viewportWidth = window.innerWidth
  const viewportHeight = window.innerHeight
  const left = Math.max(4, measured.left - padding)
  const top = Math.max(4, measured.top - padding)
  const right = Math.min(viewportWidth - 4, measured.right + padding)
  const bottom = Math.min(viewportHeight - 4, measured.bottom + padding)
  return {
    left,
    top,
    right,
    bottom,
    width: Math.max(0, right - left),
    height: Math.max(0, bottom - top),
    viewportWidth,
    viewportHeight,
  }
}

const useGuideTarget = (opened: boolean, selector?: string) => {
  const [target, setTarget] = useState<GuideTargetRect | null>(null)

  useLayoutEffect(() => {
    if (!opened) {
      setTarget(null)
      return
    }

    let frame = 0
    let scrolledTarget: HTMLElement | null = null
    const scrollPreferredTarget = () => {
      const preferredTarget = renderedTargets(selector)?.[0]
      if (!preferredTarget || isUsableTarget(preferredTarget) || preferredTarget === scrolledTarget) return

      scrolledTarget = preferredTarget
      const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches
      const targetHeight = Math.min(preferredTarget.getBoundingClientRect().height, window.innerHeight)
      const coachmarkBudget = Math.min(352, window.innerHeight * 0.44)
      const bottomNavigationAllowance = window.innerWidth <= 768 ? 70 : 0
      const canCenterBoth = targetHeight + coachmarkBudget + 12 <= window.innerHeight - bottomNavigationAllowance
      preferredTarget.scrollIntoView({
        behavior: reducedMotion ? 'auto' : 'smooth',
        block: canCenterBoth ? 'center' : 'end',
        inline: 'nearest',
      })
    }
    const update = () => {
      scrollPreferredTarget()
      window.cancelAnimationFrame(frame)
      frame = window.requestAnimationFrame(() => {
        const next = measureTarget(selector)
        setTarget((current) => (sameRect(current, next) ? current : next))
      })
    }

    update()
    const refresh = window.setInterval(update, 300)
    window.addEventListener('resize', update)
    window.addEventListener('scroll', update, true)
    return () => {
      window.cancelAnimationFrame(frame)
      window.clearInterval(refresh)
      window.removeEventListener('resize', update)
      window.removeEventListener('scroll', update, true)
    }
  }, [opened, selector])

  return target
}

const shadePath = (target: GuideTargetRect) =>
  [
    `M0 0H${target.viewportWidth}V${target.viewportHeight}H0Z`,
    `M${target.left} ${target.top}V${target.bottom}H${target.right}V${target.top}Z`,
  ].join(' ')

const coachmarkPlacement = (target: GuideTargetRect | null) => {
  if (!target) return { placement: 'center', style: undefined }

  const spaceAbove = target.top
  const spaceBelow = target.viewportHeight - target.bottom
  const placeAbove = spaceAbove >= spaceBelow
  const mobile = target.viewportWidth <= 768
  const targetOnRight = target.left + target.width / 2 > target.viewportWidth / 2
  const availableHeight = Math.max(96, (placeAbove ? spaceAbove : spaceBelow) - 12)
  const viewportHeightBudget = mobile ? target.viewportHeight * 0.44 : 352
  const style: CSSProperties = {
    position: 'fixed',
    margin: 0,
    maxHeight: Math.min(352, availableHeight, viewportHeightBudget),
    top: placeAbove ? 'auto' : target.bottom + 12,
    bottom: placeAbove ? target.viewportHeight - target.top + 12 : 'auto',
    left: mobile ? '0.5rem' : targetOnRight ? '0.75rem' : 'auto',
    right: mobile || targetOnRight ? 'auto' : '0.75rem',
    width: 'var(--modal-size)',
  }

  return { placement: placeAbove ? 'above' : 'below', style }
}

export const GuideSpotlightModal: FC<GuideSpotlightModalProps> = ({
  opened,
  onClose,
  title,
  closeLabel,
  size,
  overlayOpacity,
  targetSelector,
  children,
}) => {
  const target = useGuideTarget(opened, targetSelector)
  const [animationKey, setAnimationKey] = useState(0)
  const contentRef = useRef<HTMLDivElement>(null)
  const bodyRef = useRef<HTMLDivElement>(null)
  const coachmark = coachmarkPlacement(target)

  useEffect(() => {
    if (opened) setAnimationKey((current) => current + 1)
  }, [opened, targetSelector])

  useEffect(() => {
    if (!opened) return
    const frame = window.requestAnimationFrame(() => bodyRef.current?.focus({ preventScroll: true }))
    return () => window.cancelAnimationFrame(frame)
  }, [opened, target, targetSelector])

  useEffect(() => {
    contentRef.current?.setAttribute('aria-modal', target ? 'false' : 'true')
  }, [target])

  const targetStyle = target
    ? ({
        left: target.left,
        top: target.top,
        width: target.width,
        height: target.height,
      } satisfies CSSProperties)
    : undefined
  const cursorStyle = target
    ? ({
        left: target.left + target.width / 2,
        top: target.top + target.height / 2,
      } satisfies CSSProperties)
    : undefined
  const blockerStyles = target
    ? [
        { left: 0, top: 0, width: target.viewportWidth, height: target.top },
        {
          left: 0,
          top: target.bottom,
          width: target.viewportWidth,
          height: target.viewportHeight - target.bottom,
        },
        { left: 0, top: target.top, width: target.left, height: target.height },
        {
          left: target.right,
          top: target.top,
          width: target.viewportWidth - target.right,
          height: target.height,
        },
      ]
    : []

  return (
    <Modal.Root
      opened={opened}
      onClose={onClose}
      size={size}
      returnFocus
      trapFocus={!target}
      closeOnEscape
      closeOnClickOutside={false}
      onEnterTransitionEnd={() => bodyRef.current?.focus({ preventScroll: true })}
      zIndex={7000}
    >
      <Modal.Overlay
        data-guide-layer="fallback-overlay"
        backgroundOpacity={target ? 0 : overlayOpacity}
        blur={0}
        style={{ pointerEvents: target ? 'none' : undefined }}
      />
      {target && (
        <>
          <svg
            className={classes.tutorialShade}
            data-guide-layer="shade"
            viewBox={`0 0 ${target.viewportWidth} ${target.viewportHeight}`}
            preserveAspectRatio="none"
            aria-hidden="true"
          >
            <path
              d={shadePath(target)}
              fill={`rgb(3 7 18 / ${overlayOpacity})`}
              fillRule="evenodd"
              clipRule="evenodd"
            />
          </svg>
          {blockerStyles.map((style, index) => (
            <div
              key={index}
              className={classes.tutorialBlocker}
              data-guide-layer="interaction-blocker"
              style={style}
              aria-hidden="true"
            />
          ))}
          <div
            className={classes.tutorialSpotlight}
            data-guide-layer="spotlight"
            style={targetStyle}
            aria-hidden="true"
          />
          <div
            key={animationKey}
            className={classes.tutorialCursor}
            data-guide-layer="cursor"
            style={cursorStyle}
            aria-hidden="true"
          >
            <Icon path={mdiCursorDefaultClickOutline} size={1.7} />
          </div>
        </>
      )}
      <Modal.Content
        ref={contentRef}
        className={classes.modal}
        data-guide-surface="coachmark"
        data-guide-placement={coachmark.placement}
        style={coachmark.style}
      >
        <div className={classes.modalHeader}>
          <Modal.Title>{title}</Modal.Title>
          <Modal.CloseButton aria-label={closeLabel} />
        </div>
        <Modal.Body ref={bodyRef} className={classes.modalBody} tabIndex={0} data-autofocus aria-label={title}>
          {children}
        </Modal.Body>
      </Modal.Content>
    </Modal.Root>
  )
}
