import { Badge, Modal, Progress } from '@mantine/core'
import { mdiCursorDefaultClickOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import { CSSProperties, FC, PropsWithChildren, useEffect, useLayoutEffect, useRef, useState } from 'react'
import { coachmarkPlacement, guideLayerZIndex } from '@Utils/GuideLayout'
import type { GuideTargetRect } from '@Utils/GuideLayout'
import classes from '@Styles/PlayerGuide.module.css'

const APPLICATION_SURFACE_SELECTOR =
  '[role="dialog"], .mantine-Drawer-content, .mantine-Modal-content, .mantine-Menu-dropdown, .mantine-Popover-dropdown'

interface GuideSpotlightModalProps extends PropsWithChildren {
  opened: boolean
  onClose: () => void
  title: string
  closeLabel: string
  size: string
  overlayOpacity: number
  targetSelector?: string
  onTargetActivate?: (target: string | undefined) => void
  onTargetChange?: (target: string | undefined) => void
  showTargetCursor?: boolean
  progress?: {
    current: number
    total: number
    label: string
  }
}

const sameRect = (left: GuideTargetRect | null, right: GuideTargetRect | null) => {
  if (!left || !right) return left === right
  return (
    Math.abs(left.left - right.left) < 0.5 &&
    Math.abs(left.top - right.top) < 0.5 &&
    Math.abs(left.width - right.width) < 0.5 &&
    Math.abs(left.height - right.height) < 0.5 &&
    left.viewportWidth === right.viewportWidth &&
    left.viewportHeight === right.viewportHeight &&
    left.elevated === right.elevated &&
    left.guideTarget === right.guideTarget
  )
}

const isRenderedElement = (element: HTMLElement) => {
  const rect = element.getBoundingClientRect()
  const style = window.getComputedStyle(element)
  return rect.width > 0 && rect.height > 0 && style.display !== 'none' && style.visibility !== 'hidden'
}

export const guideTargetAcceptsKeyboardEntry = (element: HTMLElement) =>
  element.matches(
    'input:not([type="button"]):not([type="submit"]):not([type="reset"]), textarea, select, [contenteditable="true"]'
  )

export const guideTargetHasKeyboardEntryFocus = (element: HTMLElement, activeElement: Element | null) =>
  guideTargetAcceptsKeyboardEntry(element) &&
  Boolean(activeElement && (element === activeElement || element.contains(activeElement)))

const renderedTargets = (selector?: string) => {
  if (!selector) return null
  const elements = selector
    .split(',')
    .map((candidate) => candidate.trim())
    .filter(Boolean)
    .flatMap((candidate) => Array.from(document.querySelectorAll<HTMLElement>(candidate)))
  return [...new Set(elements)].filter(isRenderedElement)
}

const externalSurfaceIsOpen = () =>
  Array.from(document.querySelectorAll<HTMLElement>(APPLICATION_SURFACE_SELECTOR)).some(
    (element) => !element.closest('[data-guide-surface="coachmark"]') && isRenderedElement(element)
  )

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

const isUsableTarget = (element: HTMLElement) => targetVisibleRatio(element) >= 0.75 && targetCenterIsUsable(element)

const visibleTarget = (selector?: string) => {
  return renderedTargets(selector)?.find(isUsableTarget) ?? null
}

const targetIsElevated = (element: HTMLElement) => Boolean(element.closest(APPLICATION_SURFACE_SELECTOR))

const scrollableAncestor = (element: HTMLElement) => {
  for (let parent = element.parentElement; parent; parent = parent.parentElement) {
    const overflowY = window.getComputedStyle(parent).overflowY
    if (/(auto|scroll|overlay)/.test(overflowY) && parent.scrollHeight > parent.clientHeight + 1) return parent
  }
  return document.scrollingElement instanceof HTMLElement ? document.scrollingElement : null
}

const measureTarget = (element: HTMLElement): GuideTargetRect => {
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
    elevated: targetIsElevated(element),
    guideTarget: element.dataset.guide,
  }
}

const useGuideTarget = (opened: boolean, selector?: string) => {
  const [measurement, setMeasurement] = useState<{
    selector?: string
    target: GuideTargetRect | null
  }>({ selector, target: null })
  const target = measurement.selector === selector ? measurement.target : null

  useLayoutEffect(() => {
    if (!opened) {
      setMeasurement({ selector, target: null })
      return
    }

    let frame = 0
    let selectedTarget: HTMLElement | null = null
    let lastScrolledTarget: HTMLElement | null = null
    let lastScrolledAt = 0
    const commit = (next: GuideTargetRect | null) => {
      setMeasurement((current) => {
        if (current.selector === selector && sameRect(current.target, next)) return current
        return { selector, target: next }
      })
    }
    const stableTarget = () => {
      const targets = renderedTargets(selector) ?? []
      if (selectedTarget && (!selectedTarget.isConnected || !targets.includes(selectedTarget))) {
        selectedTarget = null
      }

      const usableTarget = targets.find(isUsableTarget) ?? null
      if (!selectedTarget) {
        selectedTarget = usableTarget ?? targets[0] ?? null
      } else if (
        usableTarget &&
        usableTarget !== selectedTarget &&
        targetIsElevated(usableTarget) &&
        !targetIsElevated(selectedTarget)
      ) {
        // A newly opened drawer, menu, or dialog owns the next real action.
        selectedTarget = usableTarget
      }
      return selectedTarget
    }
    const measureStableTarget = () => {
      const element = stableTarget()
      return element && isUsableTarget(element) ? measureTarget(element) : null
    }
    const scrollPreferredTarget = () => {
      const preferredTarget = stableTarget()
      const now = Date.now()
      if (
        !preferredTarget ||
        isUsableTarget(preferredTarget) ||
        (preferredTarget === lastScrolledTarget && now - lastScrolledAt < 800)
      ) {
        return
      }

      lastScrolledTarget = preferredTarget
      lastScrolledAt = now
      const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches
      const targetHeight = Math.min(preferredTarget.getBoundingClientRect().height, window.innerHeight)
      const coachmarkBudget = Math.min(320, window.innerHeight * 0.42)
      const bottomNavigationAllowance = window.innerWidth <= 768 ? 70 : 0
      const canCenterBoth = targetHeight + coachmarkBudget + 12 <= window.innerHeight - bottomNavigationAllowance
      preferredTarget.scrollIntoView({
        behavior: reducedMotion ? 'auto' : 'smooth',
        block: canCenterBoth ? 'center' : 'end',
        inline: 'nearest',
      })
    }
    const update = () => {
      const immediate = measureStableTarget()
      if (immediate) {
        window.cancelAnimationFrame(frame)
        commit(immediate)
        return
      }

      commit(null)
      scrollPreferredTarget()
      window.cancelAnimationFrame(frame)
      frame = window.requestAnimationFrame(() => {
        commit(measureStableTarget())
      })
    }

    update()
    const refresh = window.setInterval(update, 300)
    const observer = new MutationObserver(update)
    observer.observe(document.body, { childList: true, subtree: true })
    window.addEventListener('resize', update)
    window.addEventListener('scroll', update, true)
    return () => {
      window.cancelAnimationFrame(frame)
      window.clearInterval(refresh)
      observer.disconnect()
      window.removeEventListener('resize', update)
      window.removeEventListener('scroll', update, true)
    }
  }, [opened, selector])

  return target
}

const useExternalSurface = (opened: boolean) => {
  const [externalSurface, setExternalSurface] = useState(false)

  useLayoutEffect(() => {
    if (!opened) {
      setExternalSurface(false)
      return
    }

    const update = () =>
      setExternalSurface((current) => {
        const next = externalSurfaceIsOpen()
        return current === next ? current : next
      })
    update()
    const observer = new MutationObserver(update)
    observer.observe(document.body, { childList: true, subtree: true })
    return () => observer.disconnect()
  }, [opened])

  return externalSurface
}

const shadePath = (target: GuideTargetRect) =>
  [
    `M0 0H${target.viewportWidth}V${target.viewportHeight}H0Z`,
    `M${target.left} ${target.top}V${target.bottom}H${target.right}V${target.top}Z`,
  ].join(' ')

export const GuideSpotlightModal: FC<GuideSpotlightModalProps> = ({
  opened,
  onClose,
  title,
  closeLabel,
  size,
  overlayOpacity,
  targetSelector,
  onTargetActivate,
  onTargetChange,
  showTargetCursor = true,
  progress,
  children,
}) => {
  const target = useGuideTarget(opened, targetSelector)
  const externalSurface = useExternalSurface(opened)
  const [animationKey, setAnimationKey] = useState(0)
  const contentRef = useRef<HTMLDivElement>(null)
  const bodyRef = useRef<HTMLDivElement>(null)
  const focusedForOpening = useRef(false)
  const separationAttempt = useRef<string | null>(null)
  const coachmark = coachmarkPlacement(target)
  const guideZIndex = guideLayerZIndex(target)
  const yielding = externalSurface && !target?.elevated
  const surroundingInteractionAllowed = Boolean(
    target && visibleTarget(targetSelector)?.closest('[data-guide-interaction-scope]')
  )

  useEffect(() => {
    if (opened) setAnimationKey((current) => current + 1)
  }, [opened, target?.guideTarget, targetSelector])

  useEffect(() => {
    onTargetChange?.(opened ? target?.guideTarget : undefined)
  }, [onTargetChange, opened, target?.guideTarget])

  useEffect(() => {
    if (!opened) {
      focusedForOpening.current = false
      return
    }
    if (yielding || focusedForOpening.current) return

    focusedForOpening.current = true
    const frame = window.requestAnimationFrame(() => bodyRef.current?.focus({ preventScroll: true }))
    return () => window.cancelAnimationFrame(frame)
  }, [opened, yielding])

  useEffect(() => {
    contentRef.current?.setAttribute('aria-modal', target || yielding ? 'false' : 'true')
  }, [target, yielding])

  useLayoutEffect(() => {
    separationAttempt.current = null
  }, [opened, targetSelector])

  useLayoutEffect(() => {
    const content = contentRef.current
    if (!opened || !target || !content || yielding) return

    const separateTarget = () => {
      const contentRect = content.getBoundingClientRect()
      const overlapWidth = Math.max(
        0,
        Math.min(contentRect.right, target.right) - Math.max(contentRect.left, target.left)
      )
      const overlapHeight = Math.max(
        0,
        Math.min(contentRect.bottom, target.bottom) - Math.max(contentRect.top, target.top)
      )
      const targetArea = target.width * target.height
      if (targetArea <= 0 || (overlapWidth * overlapHeight) / targetArea <= 0.1) return

      const attemptKey = `${targetSelector ?? ''}:${target.guideTarget ?? ''}:${coachmark.placement}`
      if (separationAttempt.current === attemptKey) return
      const element = visibleTarget(targetSelector)
      if (!element) return

      separationAttempt.current = attemptKey
      const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches
      const scrollParent = scrollableAncestor(element)
      if (!scrollParent) return

      const gap = 12
      const moveTargetAbove = Math.max(0, target.bottom - contentRect.top + gap)
      const moveTargetBelow = Math.max(0, contentRect.bottom - target.top + gap)
      const roomUp = scrollParent.scrollTop
      const roomDown = scrollParent.scrollHeight - scrollParent.clientHeight - scrollParent.scrollTop
      const scrollDelta =
        roomDown >= moveTargetAbove
          ? moveTargetAbove
          : roomUp >= moveTargetBelow
            ? -moveTargetBelow
            : roomDown >= roomUp
              ? roomDown
              : -roomUp
      if (Math.abs(scrollDelta) <= 1) return
      scrollParent.scrollBy({ top: scrollDelta, behavior: reducedMotion ? 'auto' : 'smooth' })
    }

    separateTarget()
    const settledCheck = window.setTimeout(separateTarget, 250)
    return () => window.clearTimeout(settledCheck)
  }, [coachmark.placement, opened, target, targetSelector, yielding])

  useEffect(() => {
    if (!opened || !targetSelector || !onTargetActivate) return

    const activatedTarget = (eventTarget: EventTarget | null) => {
      const element = visibleTarget(targetSelector)
      if (!element || !(eventTarget instanceof Node) || !element.contains(eventTarget)) return null

      return element
    }

    const handleTargetClick = (event: MouseEvent) => {
      const element = activatedTarget(event.target)
      if (!element) return

      const guideTarget = element.dataset.guide
      onTargetActivate(guideTarget)
    }

    const handleTargetFocus = (event: FocusEvent) => {
      const element = activatedTarget(event.target)
      if (!element || !guideTargetAcceptsKeyboardEntry(element)) return

      onTargetActivate(element.dataset.guide)
    }

    document.addEventListener('click', handleTargetClick, true)
    document.addEventListener('focusin', handleTargetFocus, true)
    const focusedElement = visibleTarget(targetSelector)
    if (focusedElement && guideTargetHasKeyboardEntryFocus(focusedElement, document.activeElement)) {
      onTargetActivate(focusedElement.dataset.guide)
    }
    return () => {
      document.removeEventListener('click', handleTargetClick, true)
      document.removeEventListener('focusin', handleTargetFocus, true)
    }
  }, [onTargetActivate, opened, target?.guideTarget, targetSelector])

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
        left: Math.min(target.viewportWidth - 60, Math.max(8, target.left + target.width / 2)),
        top: Math.min(target.viewportHeight - 60, Math.max(8, target.top + target.height / 2)),
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
      trapFocus={!target && !yielding}
      lockScroll={!target && !yielding}
      closeOnEscape
      closeOnClickOutside={false}
      onEnterTransitionEnd={() => {
        if (!yielding && !focusedForOpening.current) {
          focusedForOpening.current = true
          bodyRef.current?.focus({ preventScroll: true })
        }
      }}
      zIndex={guideZIndex}
    >
      <Modal.Overlay
        data-guide-layer="fallback-overlay"
        backgroundOpacity={target || yielding ? 0 : overlayOpacity}
        blur={0}
        style={{ pointerEvents: target || yielding ? 'none' : undefined, zIndex: guideZIndex }}
      />
      {target && (
        <>
          <svg
            className={classes.tutorialShade}
            data-guide-layer="shade"
            viewBox={`0 0 ${target.viewportWidth} ${target.viewportHeight}`}
            preserveAspectRatio="none"
            aria-hidden="true"
            style={{ zIndex: guideZIndex + 1 }}
          >
            <path
              d={shadePath(target)}
              fill={`rgb(3 7 18 / ${overlayOpacity})`}
              fillRule="evenodd"
              clipRule="evenodd"
            />
          </svg>
          {!target.elevated &&
            !surroundingInteractionAllowed &&
            blockerStyles.map((style, index) => (
              <div
                key={index}
                className={classes.tutorialBlocker}
                data-guide-layer="interaction-blocker"
                style={{ ...style, zIndex: guideZIndex }}
                aria-hidden="true"
              />
            ))}
          <div
            className={classes.tutorialSpotlight}
            data-guide-layer="spotlight"
            style={{ ...targetStyle, zIndex: guideZIndex + 2 }}
            aria-hidden="true"
          />
          {showTargetCursor && (
            <div
              key={animationKey}
              className={classes.tutorialCursor}
              data-guide-layer="cursor"
              style={{ ...cursorStyle, zIndex: guideZIndex + 3 }}
              aria-hidden="true"
            >
              <Icon path={mdiCursorDefaultClickOutline} size={1.7} />
            </div>
          )}
        </>
      )}
      <Modal.Content
        ref={contentRef}
        className={classes.modal}
        data-guide-surface="coachmark"
        data-guide-placement={coachmark.placement}
        data-guide-target={target?.guideTarget}
        data-guide-yielding={yielding || undefined}
        aria-hidden={yielding || undefined}
        style={{
          ...coachmark.style,
          zIndex: guideZIndex + 4,
          visibility: yielding ? 'hidden' : undefined,
          pointerEvents: yielding ? 'none' : undefined,
        }}
      >
        <Modal.Header role="presentation" className={classes.modalHeader}>
          <div className={classes.modalHeading}>
            <Modal.Title>{title}</Modal.Title>
            {progress && (
              <Badge variant="light" size="sm" className={classes.stepBadge}>
                {progress.current} / {progress.total}
              </Badge>
            )}
          </div>
          <Modal.CloseButton aria-label={closeLabel} />
          {progress && (
            <Progress
              className={classes.modalProgress}
              size={3}
              value={(progress.current / progress.total) * 100}
              aria-label={progress.label}
            />
          )}
        </Modal.Header>
        <Modal.Body ref={bodyRef} className={classes.modalBody} tabIndex={0} data-autofocus aria-label={title}>
          {children}
        </Modal.Body>
      </Modal.Content>
    </Modal.Root>
  )
}
