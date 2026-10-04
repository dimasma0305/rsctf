import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import type { BrandingAction, ConfigEditModel } from '../Api'
import {
  dirtySettingsSections,
  ownsSettingsResult,
  settingsBrandingDigest,
  settingsRequestSignature,
} from './SettingsOperations'

test('settings mutation sends only changed sections and excludes read-only projection', () => {
  const baseline: ConfigEditModel = {
    revision: 7,
    globalConfig: { title: 'RSCTF' },
    accountPolicy: { allowRegister: true },
    proxyTrust: { enabled: true },
  }
  const current: ConfigEditModel = {
    ...baseline,
    globalConfig: { title: 'TCP1P' },
    proxyTrust: { enabled: false },
  }
  assert.deepEqual(dirtySettingsSections(baseline, current), {
    globalConfig: { title: 'TCP1P' },
  })
})

test('operation ownership fences delayed and reversed settings responses', () => {
  const owner = { operationId: 'operation-a', expectedRevision: 11, signature: 'request-a' }
  assert.equal(ownsSettingsResult(owner, { operationId: 'operation-a', revision: 12, brandingHash: null }), true)
  assert.equal(ownsSettingsResult(owner, { operationId: 'operation-b', revision: 12, brandingHash: null }), false)
  assert.equal(ownsSettingsResult(owner, { operationId: 'operation-a', revision: 13, brandingHash: null }), false)
})

test('request signature includes the branding disposition', () => {
  const request: ConfigEditModel = {
    brandingAction: 'Keep' as BrandingAction,
    globalConfig: { title: 'RSCTF' },
    email: { password: 'smtp-secret' },
  }
  const signature = settingsRequestSignature(request)
  assert.match(signature, /^[0-9a-f]{64}$/)
  assert.doesNotMatch(signature, /smtp-secret/)
  assert.notEqual(signature, settingsRequestSignature({ ...request, brandingAction: 'Clear' as BrandingAction }))
})

test('request signature rotates when staged branding bytes change', async () => {
  const request: ConfigEditModel = { brandingAction: 'Set' as BrandingAction }
  const first = await settingsBrandingDigest(new Blob(['first']))
  const second = await settingsBrandingDigest(new Blob(['second']))
  assert.notEqual(first, second)
  assert.notEqual(settingsRequestSignature(request, first), settingsRequestSignature(request, second))
})

test('settings page owns one operation and reconciles branding with the same intent', () => {
  const source = readFileSync('src/pages/admin/Settings.tsx', 'utf8')
  assert.match(source, /saveOwnerRef\.current = true/)
  assert.match(source, /if \(saveOwnerRef\.current \|\| !configs \|\| !dirty\) return/)
  assert.match(source, /dirtySettingsSections\(initialSnapshotRef\.current, currentSnapshot\)/)
  assert.match(source, /adminStageSettingsBranding\(owner\.operationId/)
  assert.match(source, /adminGetSettingsOperation\(owner\.operationId\)/)
  assert.match(source, /storeSettingsOperation\(owner\)/)
  assert.match(source, /clearSettingsOperation\(\)/)
  assert.match(source, /Promise\.allSettled\(\[mutate\(\), mutateConfig\(\), mutateCaptchaConfig\(\)\]\)/)
})
