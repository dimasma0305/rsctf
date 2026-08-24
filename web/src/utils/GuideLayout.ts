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
}

export const GUIDE_PAGE_Z_INDEX = 150
export const GUIDE_ELEVATED_Z_INDEX = 500
export const MOBILE_TOP_SAFE_INSET = 76
export const MOBILE_BOTTOM_SAFE_INSET = 82

export const guideLayerZIndex = (target: GuideTargetRect | null) =>
  target?.elevated ? GUIDE_ELEVATED_Z_INDEX : GUIDE_PAGE_Z_INDEX

export const coachmarkPlacement = (target: GuideTargetRect | null) => {
  if (!target) return { placement: 'center', style: undefined }

  const mobile = target.viewportWidth <= 768
  const safeTop = mobile ? MOBILE_TOP_SAFE_INSET : 12
  const safeBottom = mobile ? target.viewportHeight - MOBILE_BOTTOM_SAFE_INSET : target.viewportHeight - 12
  const gap = 12
  const spaceAbove = Math.max(0, target.top - safeTop - gap)
  const spaceBelow = Math.max(0, safeBottom - target.bottom - gap)
  const placeAbove = spaceAbove >= spaceBelow
  const targetOnRight = target.left + target.width / 2 > target.viewportWidth / 2
  const availableHeight = Math.max(96, placeAbove ? spaceAbove : spaceBelow)
  const viewportHeightBudget = mobile ? target.viewportHeight * 0.44 : 352
  const style: CSSProperties = {
    position: 'fixed',
    margin: 0,
    maxHeight: Math.min(352, availableHeight, viewportHeightBudget),
    top: placeAbove ? 'auto' : target.bottom + gap,
    bottom: placeAbove ? target.viewportHeight - target.top + gap : 'auto',
    left: mobile ? '0.5rem' : targetOnRight ? '0.75rem' : 'auto',
    right: mobile || targetOnRight ? 'auto' : '0.75rem',
    width: 'var(--modal-size)',
  }

  return { placement: placeAbove ? 'above' : 'below', style }
}
