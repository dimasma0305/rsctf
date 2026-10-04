import type { CSSProperties } from 'react'

export interface GuideTargetRect {
  left: number
  top: number
  right: number
  bottom: number
  width: number
  height: number
  viewportWidth: number
  viewportHeight: number
  elevated: boolean
  guideTarget?: string
}

export const GUIDE_PAGE_Z_INDEX = 150
export const GUIDE_ELEVATED_Z_INDEX = 500
export const DESKTOP_TOP_SAFE_INSET = 60
export const DESKTOP_BOTTOM_SAFE_INSET = 12
export const MOBILE_TOP_SAFE_INSET = 76
export const MOBILE_BOTTOM_SAFE_INSET = 82

export const guideLayerZIndex = (target: GuideTargetRect | null) =>
  target?.elevated ? GUIDE_ELEVATED_Z_INDEX : GUIDE_PAGE_Z_INDEX

export const coachmarkPlacement = (target: GuideTargetRect | null) => {
  if (!target) {
    return {
      placement: 'docked',
      style: {
        position: 'fixed',
        margin: 0,
        top: MOBILE_TOP_SAFE_INSET,
        bottom: 'auto',
        left: 'auto',
        right: '0.5rem',
        width: 'var(--modal-size)',
        maxHeight: `min(20rem, calc(100dvh - ${MOBILE_TOP_SAFE_INSET + MOBILE_BOTTOM_SAFE_INSET}px))`,
      } satisfies CSSProperties,
    }
  }

  const mobile = target.viewportWidth <= 768
  const safeTop = mobile ? MOBILE_TOP_SAFE_INSET : DESKTOP_TOP_SAFE_INSET
  const bottomInset = mobile ? MOBILE_BOTTOM_SAFE_INSET : DESKTOP_BOTTOM_SAFE_INSET
  const safeBottom = target.viewportHeight - bottomInset
  const upperRoom = Math.max(0, target.top - safeTop - 12)
  const lowerRoom = Math.max(0, safeBottom - target.bottom - 12)
  const dockAtTop = upperRoom >= lowerRoom
  const targetOnRight = target.left + target.width / 2 > target.viewportWidth / 2
  const availableHeight = Math.max(160, safeBottom - safeTop)
  const viewportHeightBudget = mobile ? Math.min(288, target.viewportHeight * 0.42) : 320
  const targetSideBudget = dockAtTop ? upperRoom : lowerRoom
  const style: CSSProperties = {
    position: 'fixed',
    margin: 0,
    maxHeight: Math.max(160, Math.min(320, availableHeight, viewportHeightBudget, targetSideBudget)),
    top: dockAtTop ? safeTop : 'auto',
    bottom: dockAtTop ? 'auto' : bottomInset,
    left: mobile ? '0.5rem' : targetOnRight ? '0.75rem' : 'auto',
    right: mobile || targetOnRight ? 'auto' : '0.75rem',
    width: 'var(--modal-size)',
  }

  const horizontalPlacement = mobile ? 'wide' : targetOnRight ? 'left' : 'right'
  return { placement: `${dockAtTop ? 'top' : 'bottom'}-${horizontalPlacement}`, style }
}
