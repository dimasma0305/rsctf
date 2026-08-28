import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

import type { BrandingAction, ConfigEditModel } from '../Api'
import {
  clearSettingsOperation,
  dirtySettingsSections,
  ownsSettingsResult,
  readSettingsOperation,
  retainSettingsOperation,
  settingsRequestSignature,
  type SettingsOperationOwner,
  type SettingsOperationStorage,
} from './SettingsOperations'

class MemoryStorage implements SettingsOperationStorage {
  readonly values = new Map<string, string>()

  getItem(key: string): string | null {
    return this.values.get(key) ?? null
  }

  setItem(key: string, value: string): void {
    this.values.set(key, value)
  }

  removeItem(key: string): void {
    this.values.delete(key)
  }
}

const retainedOperation = (createdAt = Date.now()): SettingsOperationOwner => ({
  operationId: '0c41b24a-ea80-4f17-9f8b-37ee75d3ff65',
  expectedRevision: 11,
  signature: 'request-a',
  createdAt,
})

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
  const owner = retainedOperation()
  assert.equal(
    ownsSettingsResult(owner, { operationId: owner.operationId, revision: 12, brandingHash: null }),
    true
  )
  assert.equal(
    ownsSettingsResult(owner, { operationId: 'operation-b', revision: 12, brandingHash: null }),
    false
  )
  assert.equal(
    ownsSettingsResult(owner, { operationId: owner.operationId, revision: 13, brandingHash: null }),
    false
  )
})

test('ambiguous settings operation survives reload and is cleared only by its owner', () => {
  const storage = new MemoryStorage()
  const owner = retainedOperation()
  retainSettingsOperation(storage, owner)

  assert.deepEqual(readSettingsOperation(storage, owner.createdAt + 1), { ...owner, signature: '' })
  assert.doesNotMatch([...storage.values.values()][0], /request-a/)
  clearSettingsOperation(storage, 'f9accde2-ccf7-492d-a526-9b7fa142b7cc')
  assert.deepEqual(readSettingsOperation(storage, owner.createdAt + 2), { ...owner, signature: '' })
  clearSettingsOperation(storage, owner.operationId)
  assert.equal(readSettingsOperation(storage, owner.createdAt + 3), null)
})

test('expired or malformed settings operation cannot become a mutation identity', () => {
  const storage = new MemoryStorage()
  const owner = retainedOperation(10_000)
  retainSettingsOperation(storage, owner)
  assert.equal(readSettingsOperation(storage, owner.createdAt + 60 * 60 * 1_000 + 1), null)
  assert.equal(storage.values.size, 0)

  storage.setItem('rsctf:admin:platform-settings-operation', '{"operationId":"forged"}')
  assert.equal(readSettingsOperation(storage, owner.createdAt), null)
  assert.equal(storage.values.size, 0)
})

test('request signature includes the branding disposition', () => {
  const request: ConfigEditModel = { brandingAction: 'Keep' as BrandingAction, globalConfig: { title: 'RSCTF' } }
  assert.notEqual(
    settingsRequestSignature(request),
    settingsRequestSignature({ ...request, brandingAction: 'Clear' as BrandingAction })
  )
})

test('settings page owns one operation and reconciles branding with the same intent', () => {
  const source = readFileSync('src/pages/admin/Settings.tsx', 'utf8')
  assert.match(source, /saveOwnerRef\.current = true/)
  assert.match(source, /if \(saveOwnerRef\.current \|\| !configs \|\| !dirty\) return/)
  assert.match(source, /dirtySettingsSections\(initialSnapshotRef\.current, currentSnapshot\)/)
  assert.match(source, /adminStageSettingsBranding\(owner\.operationId/)
  assert.match(source, /adminGetSettingsOperation\(owner\.operationId\)/)
  assert.match(source, /retainSettingsOperationSafely\(owner\)/)
  assert.match(source, /loadRetainedSettingsOperation\(\)/)
  assert.match(source, /Promise\.allSettled\(\[mutate\(\), mutateConfig\(\), mutateCaptchaConfig\(\)\]\)/)
})
