import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { Role, type ProfileUserInfoModel } from '../Api'
import {
  getAdminNavigation,
  getWorkspaceNavigation,
  isNavigationItemActive,
  PRIMARY_NAVIGATION,
  ADMIN_NAVIGATION,
} from '../components/navigation'

const user = (role: Role, hasManagedGames = false) => ({ role, hasManagedGames }) as ProfileUserInfoModel

test('workspace navigation keeps anonymous, player and event-manager boundaries', () => {
  assert.deepEqual(getAdminNavigation(), [])
  assert.deepEqual(getAdminNavigation(user(Role.User)), [])
  assert.deepEqual(
    getAdminNavigation(user(Role.User, true)).map((item) => item.path),
    ['games']
  )
  assert.equal(getAdminNavigation(user(Role.Admin)).length, ADMIN_NAVIGATION.length)
  assert.ok(!getWorkspaceNavigation('/games').some((item) => item.requiresAuth || item.admin || item.requiresDonations))
  assert.ok(getWorkspaceNavigation('/games', user(Role.User)).some((item) => item.link === '/challenges'))
  const manager = getWorkspaceNavigation('/admin/games/19/info', user(Role.User, true))
  assert.deepEqual(
    manager.map((item) => item.link),
    ['/admin/games']
  )
})

test('navigation selection uses complete route segments and one admin destination', () => {
  const teams = PRIMARY_NAVIGATION.find((item) => item.link === '/teams')!
  assert.equal(isNavigationItemActive(teams, '/teams'), true)
  assert.equal(isNavigationItemActive(teams, '/teams-extra'), false)
  const items = getWorkspaceNavigation('/admin/games/19/info', user(Role.Admin))
  assert.deepEqual(
    items.filter((item) => isNavigationItemActive(item, '/admin/games/19/info')).map((item) => item.link),
    ['/admin/games']
  )
})

test('quick navigation owns its keyboard listener and yields to existing dialogs', () => {
  const source = readFileSync('src/components/WorkspaceBar.tsx', 'utf8')
  assert.match(source, /getAdminNavigation\(user\)/)
  assert.match(source, /canAccessNavigationItem/)
  assert.match(source, /removeEventListener\('keydown', onKeyDown\)/)
  assert.match(source, /querySelectorAll\('\[role="dialog"\], dialog\[open\]'\)/)
  assert.match(source, /dialog\.getClientRects\(\)\.length > 0/)
  assert.match(source, /data-autofocus/)
  assert.match(source, /aria-haspopup="dialog"/)
})

test('settings tabs expose orientation and respect reduced-motion preferences', () => {
  const tabs = readFileSync('src/components/IconTabs.tsx', 'utf8')
  assert.match(tabs, /aria-orientation=/)
  assert.match(tabs, /ArrowUp/)
  assert.match(tabs, /ArrowDown/)
  assert.match(tabs, /reducedMotion \? 'auto' : 'smooth'/)
  const settings = readFileSync('src/pages/admin/Settings.tsx', 'utf8')
  assert.match(settings, /orientation="vertical"/)
  assert.match(settings, /hidden=\{!dirty && saved\}/)
})

test('challenge browsing has resettable search and uses natural page height', () => {
  const panel = readFileSync('src/components/ChallengePanel.tsx', 'utf8')
  const toolbar = readFileSync('src/components/competition/ChallengeToolbar.tsx', 'utf8')
  assert.match(panel, /<ChallengeToolbar/)
  assert.match(toolbar, /search_challenges/)
  assert.match(panel, /reset_filters/)
  assert.doesNotMatch(panel, /h=\{isCompact \? undefined : 'calc\(100vh - 6\.67rem\)'\}/)
  const card = readFileSync('src/components/ChallengeCard.tsx', 'utf8')
  assert.match(card, /type="button"/)
  assert.match(card, /common\.workspace\.solved/)
  assert.match(card, /common\.workspace\.live_scoring/)
  assert.doesNotMatch(card, /<ScrollingText|classes\.icon/)
})
