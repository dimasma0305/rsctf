import assert from 'node:assert/strict'
import test from 'node:test'
import {
  DESKTOP_BOTTOM_SAFE_INSET,
  DESKTOP_TOP_SAFE_INSET,
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

test('coach marks stay docked while a highlighted target is resolving', () => {
  const docked = coachmarkPlacement(null)
  assert.equal(docked.placement, 'docked')
  assert.equal(docked.style?.position, 'fixed')
  assert.equal(docked.style?.top, MOBILE_TOP_SAFE_INSET)
  assert.equal(docked.style?.right, '0.5rem')
  assert.equal(docked.style?.left, 'auto')
  assert.match(String(docked.style?.maxHeight), /100dvh/)
})

test('mobile coach marks stay clear of the header and bottom navigation', () => {
  const bottomDocked = coachmarkPlacement(mobileTarget(120, 184))
  assert.equal(bottomDocked.placement, 'bottom-wide')
  assert.equal(bottomDocked.style?.top, 'auto')
  assert.equal(bottomDocked.style?.bottom, MOBILE_BOTTOM_SAFE_INSET)
  assert.ok(
    Number(bottomDocked.style?.maxHeight) <= 844 - MOBILE_TOP_SAFE_INSET - MOBILE_BOTTOM_SAFE_INSET,
    'coach mark must fit between the persistent mobile controls'
  )

  const topDocked = coachmarkPlacement(mobileTarget(770, 838))
  assert.equal(topDocked.placement, 'top-wide')
  assert.equal(topDocked.style?.top, MOBILE_TOP_SAFE_INSET)
  assert.equal(topDocked.style?.bottom, 'auto')

  const compactMiddleTarget: GuideTargetRect = {
    ...mobileTarget(284, 336),
    right: 280,
    width: 240,
    viewportWidth: 320,
    viewportHeight: 568,
  }
  const compactTop = coachmarkPlacement(compactMiddleTarget)
  assert.equal(compactTop.placement, 'top-wide')
  assert.ok(
    Number(compactTop.style?.maxHeight) <= compactMiddleTarget.top - MOBILE_TOP_SAFE_INSET - 12,
    'a compact coach mark must leave a visible gap before its target'
  )
})

test('desktop coach marks stay in the opposite safe corner instead of jumping into page content', () => {
  const target: GuideTargetRect = {
    ...mobileTarget(100, 160),
    left: 40,
    right: 240,
    width: 200,
    viewportWidth: 1440,
    viewportHeight: 1100,
  }
  const bottomRight = coachmarkPlacement(target)
  assert.equal(bottomRight.placement, 'bottom-right')
  assert.equal(bottomRight.style?.right, '0.75rem')
  assert.equal(bottomRight.style?.bottom, DESKTOP_BOTTOM_SAFE_INSET)

  const topLeft = coachmarkPlacement({
    ...target,
    left: 1120,
    right: 1360,
    top: 900,
    bottom: 960,
  })
  assert.equal(topLeft.placement, 'top-left')
  assert.equal(topLeft.style?.left, '0.75rem')
  assert.equal(topLeft.style?.top, DESKTOP_TOP_SAFE_INSET)
})
