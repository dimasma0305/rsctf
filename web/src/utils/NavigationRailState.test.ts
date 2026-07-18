import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  deserializeNavigationRailPreference,
  getNavigationRailWidth,
  NAVIGATION_COMPACT_MEDIA_QUERY,
  NAVIGATION_RAIL_WIDTH,
  NAVIGATION_SIDEBAR_WIDTH,
  resolveNavigationRailCompact,
  serializeNavigationRailPreference,
  toggleNavigationRailPreference,
} from './NavigationRailState'

test('navigation rail compact query ends before the 75em expanded breakpoint', () => {
  assert.equal(NAVIGATION_COMPACT_MEDIA_QUERY, '(min-width: 48em) and (max-width: 74.99em)')
})

test('navigation rail defaults to the responsive shell mode when no preference exists', () => {
  assert.equal(resolveNavigationRailCompact(null, false), false)
  assert.equal(resolveNavigationRailCompact(null, true), true)
})

test('an explicit navigation rail preference overrides the responsive default', () => {
  assert.equal(resolveNavigationRailCompact(true, false), true)
  assert.equal(resolveNavigationRailCompact(false, true), false)
})

test('navigation rail storage accepts booleans and safely rejects malformed values', () => {
  assert.equal(deserializeNavigationRailPreference('true'), true)
  assert.equal(deserializeNavigationRailPreference('false'), false)

  for (const value of [undefined, 'null', '1', '0', '"true"', 'compact', '{}', '[]']) {
    assert.equal(deserializeNavigationRailPreference(value), null, String(value))
  }

  assert.equal(deserializeNavigationRailPreference(serializeNavigationRailPreference(true)), true)
  assert.equal(deserializeNavigationRailPreference(serializeNavigationRailPreference(false)), false)
})

test('navigation rail toggle and widths map to the rendered shell state', () => {
  assert.equal(toggleNavigationRailPreference(false), true)
  assert.equal(toggleNavigationRailPreference(true), false)
  assert.equal(getNavigationRailWidth(false), NAVIGATION_SIDEBAR_WIDTH)
  assert.equal(getNavigationRailWidth(true), NAVIGATION_RAIL_WIDTH)
})
