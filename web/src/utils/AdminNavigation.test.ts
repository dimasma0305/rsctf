import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { Role, type ProfileUserInfoModel } from '../Api'
import {
  getAdminEventContext,
  getEventAdminSections,
  getSettingsSection,
  SETTINGS_SECTIONS,
} from '../components/admin/navigation'

test('event navigation matches complete segments and distinguishes reviews, pending and flags', () => {
  for (const path of ['info', 'adops', 'review', 'challengereviews', 'pending'])
    assert.equal(getAdminEventContext(`/admin/games/19/${path}`)?.section?.path, path)
  assert.equal(getAdminEventContext('/admin/games/19/challenges/pending')?.section?.path, 'pending')
  assert.equal(getAdminEventContext('/admin/games/19/challenges/74/flags')?.challengeId, '74')
  assert.equal(getAdminEventContext('/admin/games/19/challenges/74/flags')?.flags, true)
  assert.equal(getAdminEventContext('/admin/games/19/challengereviews-extra')?.section, undefined)
  assert.equal(getAdminEventContext('/admin/games'), null)
  assert.equal(getAdminEventContext('/games/19/challenges'), null)
})

test('event navigation and section shortcuts retain administrator visibility boundaries', () => {
  assert.deepEqual(getEventAdminSections(), [])
  assert.deepEqual(getEventAdminSections({ role: Role.User } as ProfileUserInfoModel), [])
  const manager = getEventAdminSections({ role: Role.User, hasManagedGames: true } as ProfileUserInfoModel)
  assert.ok(manager.some((item) => item.path === 'adops'))
  assert.ok(!manager.some((item) => item.path === 'managers'))
  assert.ok(
    getEventAdminSections({ role: Role.Admin } as ProfileUserInfoModel).some((item) => item.path === 'managers')
  )
})

test('settings section URLs allow only known sections and fall back safely', () => {
  for (const { key } of SETTINGS_SECTIONS) assert.equal(getSettingsSection(`?section=${key}`), key)
  assert.equal(getSettingsSection('?section=unknown'), 'platform')
  assert.equal(getSettingsSection('?section=https://example.invalid'), 'platform')
  assert.equal(getSettingsSection(''), 'platform')
})

test('shared navigation removes duplicate console tabs, keeps mobile admin scope and preserves settings drafts', () => {
  const event = readFileSync('src/components/admin/WithGameEditTab.tsx', 'utf8')
  assert.doesNotMatch(event, /ChallengeConsoleTabs|path.includes|setActiveTab/)
  assert.match(event, /headerActions=/)
  assert.match(event, /getEventAdminSections\(user\)/)
  const mobile = readFileSync('src/components/AppHeader.tsx', 'utf8')
  assert.match(mobile, /const dockItems = adminWorkspace/)
  const settings = readFileSync('src/pages/admin/Settings.tsx', 'utf8')
  assert.match(settings, /useSearchParams\(\)/)
  assert.match(settings, /new URLSearchParams\(previous\)/)
  assert.match(settings, /preventScrollReset: true/)
  const switcher = readFileSync('src/components/admin/WithChallengeEdit.tsx', 'utf8')
  assert.match(switcher, /data-challenge-switcher/)
  assert.doesNotMatch(switcher, /calc\(100vh/)
})
