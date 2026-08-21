import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import type { AccountPolicy } from '@Api'
import { getAccountUniquenessState, setBrowserFingerprintCollection } from './settingsAccountPolicy'

test('account status recognizes every effective uniqueness policy', () => {
  const effectivePolicies: (keyof AccountPolicy)[] = [
    'requireUniqueIpPerTeamUser',
    'requireUniqueIpGlobal',
    'requireUniqueFingerprintPerTeamUser',
    'requireUniqueFingerprintGlobal',
  ]

  for (const policy of effectivePolicies) {
    const state = getAccountUniquenessState({
      [policy]: true,
      enableBrowserFingerprint: policy.toLowerCase().includes('fingerprint'),
    })

    assert.equal(state.hasEffectiveUniquenessPolicy, true, policy)
    assert.equal(state.hasIneffectiveFingerprintPolicy, false, policy)
    assert.equal(state.status, 'configured', policy)
  }

  assert.equal(getAccountUniquenessState(undefined).status, 'attention')
})

test('fingerprint uniqueness policies are ineffective without fingerprint collection', () => {
  for (const policy of ['requireUniqueFingerprintPerTeamUser', 'requireUniqueFingerprintGlobal'] as const) {
    const state = getAccountUniquenessState({ [policy]: true, enableBrowserFingerprint: false })

    assert.equal(state.fingerprintCollectionEnabled, false, policy)
    assert.equal(state.hasEffectiveUniquenessPolicy, false, policy)
    assert.equal(state.hasIneffectiveFingerprintPolicy, true, policy)
    assert.equal(state.status, 'attention', policy)
  }
})

test('an ineffective fingerprint policy keeps account status at attention even with an effective IP policy', () => {
  const state = getAccountUniquenessState({
    requireUniqueIpGlobal: true,
    requireUniqueFingerprintGlobal: true,
    enableBrowserFingerprint: false,
  })

  assert.equal(state.hasEffectiveUniquenessPolicy, true)
  assert.equal(state.hasIneffectiveFingerprintPolicy, true)
  assert.equal(state.status, 'attention')
})

test('turning fingerprint collection off clears both dependent policies only', () => {
  const policy = setBrowserFingerprintCollection(
    {
      allowRegister: true,
      requireUniqueIpPerTeamUser: true,
      requireUniqueIpGlobal: true,
      requireUniqueFingerprintPerTeamUser: true,
      requireUniqueFingerprintGlobal: true,
    },
    false
  )

  assert.equal(policy.enableBrowserFingerprint, false)
  assert.equal(policy.requireUniqueFingerprintPerTeamUser, false)
  assert.equal(policy.requireUniqueFingerprintGlobal, false)
  assert.equal(policy.requireUniqueIpPerTeamUser, true)
  assert.equal(policy.requireUniqueIpGlobal, true)
  assert.equal(policy.allowRegister, true)
})

test('Settings disables both fingerprint policy switches and explains their dependency', () => {
  const settings = readFileSync('src/pages/admin/Settings.tsx', 'utf8')
  const disabledDependency = /disabled=\{disabled \|\| !accountUniqueness\.fingerprintCollectionEnabled\}/g
  const inactiveCopy = /admin\.content\.settings\.account\.browser_fingerprint\.policy_inactive/g

  assert.equal((settings.match(disabledDependency) ?? []).length, 2)
  assert.equal((settings.match(inactiveCopy) ?? []).length, 2)
  assert.match(
    settings,
    /setAccountPolicy\(setBrowserFingerprintCollection\(accountPolicy, e\.currentTarget\.checked\)\)/
  )
  assert.match(settings, /accountUniqueness\.hasIneffectiveFingerprintPolicy/)
})

test('English settings copy describes inactive fingerprint policies', () => {
  const locale = JSON.parse(readFileSync('src/locales/en-US/admin.json', 'utf8'))
  const fingerprint = locale.content.settings.account.browser_fingerprint

  assert.match(fingerprint.policy_inactive, /Inactive.*Browser Fingerprinting is off/i)
  assert.match(fingerprint.policies_ineffective, /configured but inactive/i)
})

test('Settings exposes OAuth-only registration with safe provider guidance', () => {
  const settings = readFileSync('src/pages/admin/Settings.tsx', 'utf8')
  const register = readFileSync('src/pages/account/Register.tsx', 'utf8')
  const locale = JSON.parse(readFileSync('src/locales/en-US/admin.json', 'utf8'))

  assert.match(settings, /allowPasswordRegistration/)
  assert.match(settings, /oauthOnlyRegistrationNeedsAttention/)
  assert.match(settings, /\/api\/oauth\/google\/callback/)
  assert.match(settings, /\/api\/oauth\/discord\/callback/)
  assert.match(locale.content.settings.account.allow_password_registration.description, /Existing password accounts/i)
  assert.match(register, /!bootstrapMode && config\.allowPasswordRegistration === false/)
  assert.match(register, /providerAvailable && <OAuthButtons \/>/)
})
