import assert from 'node:assert/strict'
import test from 'node:test'
import {
  GUIDE_ELEVATED_Z_INDEX,
  GUIDE_PAGE_Z_INDEX,
  MOBILE_BOTTOM_SAFE_INSET,
  MOBILE_TOP_SAFE_INSET,
  coachmarkPlacement,
  guideLayerZIndex,
} from './GuideLayout'
import type { GuideTargetRect } from './GuideLayout'

const mobileTarget = (top: number, bottom: number, elevated = false): GuideTargetRect => ({
  left: 40,
  top,
  right: 350,
  bottom,
  width: 310,
  height: bottom - top,
  viewportWidth: 390,
  viewportHeight: 844,
  elevated,
})

test('guide layers yield to opened surfaces and rise for targets inside them', () => {
  assert.equal(guideLayerZIndex(mobileTarget(200, 250)), GUIDE_PAGE_Z_INDEX)
  assert.equal(guideLayerZIndex(mobileTarget(200, 250, true)), GUIDE_ELEVATED_Z_INDEX)
  assert.ok(GUIDE_PAGE_Z_INDEX < 200, 'page guide must stay below Mantine dialogs')
  assert.ok(GUIDE_ELEVATED_Z_INDEX > 400, 'nested guide must clear Mantine overlays')
})

test('mobile coach marks stay clear of the header and bottom navigation', () => {
  const below = coachmarkPlacement(mobileTarget(285, 349))
  assert.equal(below.placement, 'below')
  assert.equal(below.style?.top, 361)
  assert.ok(
    Number(below.style?.top) + Number(below.style?.maxHeight) <= 844 - MOBILE_BOTTOM_SAFE_INSET,
    'coach mark must end above the mobile dock'
  )

  const above = coachmarkPlacement(mobileTarget(770, 838))
  assert.equal(above.placement, 'above')
  assert.ok(
    844 - Number(above.style?.bottom) - Number(above.style?.maxHeight) >= MOBILE_TOP_SAFE_INSET,
    'coach mark must start below the mobile header'
  )
})
