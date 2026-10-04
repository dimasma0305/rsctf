import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { profileFields, refreshedProfileDraft, sameProfile } from './ProfileDraft'
import { routeLifecycleKey } from './ViewerIdentity'

const user = { userId: 'one', userName: 'Player', bio: null, phone: null }

test('profile tabs preserve drafts without weakening path, account, or other query boundaries', () => {
  const key = (path: string, search: string, scope = 'user:one:User') => routeLifecycleKey(path, search, scope)
  assert.equal(key('/account/profile', ''), key('/account/profile', '?tab=stats'))
  assert.equal(key('/account/profile', '?other=1'), key('/account/profile', '?other=1&tab=stats'))
  assert.notEqual(key('/account/profile', ''), key('/account/profile', '?other=1'))
  assert.notEqual(key('/account/profile', '?tab=stats'), key('/account/profile', '?tab=stats', 'user:two:User'))
  assert.notEqual(key('/account/profile', '?tab=stats'), key('/account/profile', '?tab=stats', 'anonymous'))
  assert.notEqual(key('/account/profile', ''), key('/account/stats', ''))
  assert.notEqual(key('/games/19/challenges', ''), key('/games/19/challenges', '?tab=stats'))
})

test('empty profile values remain empty and contain no placeholder text or account privileges', () => {
  assert.deepEqual(profileFields(user), { userName: 'Player', bio: '', phone: '', realName: '', stdNumber: '' })
  assert.ok(sameProfile(profileFields(user), profileFields({ ...user, bio: '' })))
})

test('avatar and email refreshes preserve unsaved profile fields', () => {
  const draft = { ...profileFields(user), bio: 'Unfinished edit' }
  const refreshed = { ...user, email: 'new@example.invalid', avatar: '/avatar.png' }
  assert.deepEqual(refreshedProfileDraft(draft, user, refreshed), draft)
  assert.equal(sameProfile(draft, profileFields(refreshed)), false)
})

test('clean profiles follow refreshed server data and never retain another user’s draft', () => {
  const next = { ...user, userName: 'Updated' }
  assert.deepEqual(refreshedProfileDraft(profileFields(user), user, next), profileFields(next))
  const replacement = { userId: 'two', userName: 'Other player' }
  assert.deepEqual(
    refreshedProfileDraft({ ...profileFields(user), bio: 'Private draft' }, user, replacement),
    profileFields(replacement)
  )
  assert.deepEqual(refreshedProfileDraft(profileFields(user), user, undefined), profileFields())
})

const source = readFileSync('src/pages/account/Profile.tsx', 'utf8')
test('profile forms retain guide targets, server APIs, and keyboard-submit semantics', () => {
  assert.match(source, /<PageHeader/)
  assert.match(source, /data-guide="account-access"/)
  assert.match(source, /data-profile-form/)
  assert.match(source, /type="submit" disabled=\{disabled \|\| !dirty\}/)
  assert.match(source, /api.account.accountUpdate\(submitted\)/)
  assert.match(source, /profileInFlight.current = true/)
  assert.match(source, /profileInFlight.current = false/)
  assert.match(source, /revalidate: false/)
  assert.doesNotMatch(source, /mutate\(\{ \.\.\.user \}\)/)
  assert.match(source, /role="status" aria-live="polite"/)
  assert.match(source, /autoComplete="current-password"/)
  assert.match(source, /const closeEmail = \(\) => \{[\s\S]*?setEmailPassword\(''\)/)
  assert.match(source, /disabled=\{disabled \|\| !avatarFile\}/)
  assert.match(source, /multiple=\{false\}/)
  assert.match(source, /inputProps=\{\{ 'aria-label': avatarModalTitle \}\}/)
  assert.equal(source.match(/closeButtonProps=\{\{ 'aria-label': t\('account.profile_ui.close'\)/g)?.length, 3)
})

test('profile copy includes matching English and Indonesian keys', () => {
  const en = JSON.parse(readFileSync('src/locales/en-US/account.json', 'utf8'))
  const id = JSON.parse(readFileSync('src/locales/id-ID/account.json', 'utf8'))
  assert.deepEqual(Object.keys(en.profile_ui).sort(), Object.keys(id.profile_ui).sort())
  for (const locale of [en, id]) {
    for (const value of Object.values(locale.profile_ui)) assert.ok(typeof value === 'string' && value.length > 0)
    assert.ok(locale.button.change_avatar)
    assert.ok(locale.title.stats)
  }
})
