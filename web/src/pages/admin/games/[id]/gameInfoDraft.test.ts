import assert from 'node:assert/strict'
import test from 'node:test'
import type { GameInfoModel } from '@Api'
import { buildGameInfoUpdatePayload, gameInfoDraftChanged, prepareGameInfoSave } from './gameInfoDraft'

const saved: GameInfoModel = {
  id: 7,
  title: 'Practice event',
  inviteCode: null,
  start: 1_000,
  end: 2_000,
  freeze: null,
  writeupDeadline: 2_000,
  vpnAccessRequired: false,
  vpnPolicyChangeReason: null,
}

const schedule = {
  start: saved.start,
  end: saved.end,
  freeze: saved.freeze ?? null,
  writeupDeadline: saved.writeupDeadline!,
}

test('an unchanged game info form does not become dirty', () => {
  const baseline = buildGameInfoUpdatePayload(
    { ...saved, operationId: 'server-owned-value', serverTime: 1_000 },
    schedule,
    false
  )
  const current = buildGameInfoUpdatePayload({ ...saved, operationId: null, serverTime: 2_000 }, schedule, false)

  assert.equal(gameInfoDraftChanged(current, baseline), false)
  assert.equal('serverTime' in current, false)
  assert.equal('operationId' in current, false)
})

test('schedule and ordinary field edits make the form dirty', () => {
  const baseline = buildGameInfoUpdatePayload(saved, schedule, false)
  const rescheduled = buildGameInfoUpdatePayload(saved, { ...schedule, start: 1_500 }, false)
  const renamed = buildGameInfoUpdatePayload({ ...saved, title: 'Renamed event' }, schedule, false)

  assert.equal(gameInfoDraftChanged(rescheduled, baseline), true)
  assert.equal(gameInfoDraftChanged(renamed, baseline), true)
})

test('an unused VPN audit reason does not create a no-op save', () => {
  const baseline = buildGameInfoUpdatePayload(saved, schedule, false)
  const current = buildGameInfoUpdatePayload(
    { ...saved, vpnPolicyChangeReason: 'reason left from a reverted toggle' },
    schedule,
    false
  )

  assert.equal(gameInfoDraftChanged(current, baseline), false)
})

test('a settings save keeps its operation ID for retries and rotates it after an edit', () => {
  let sequence = 0
  const createId = () => `operation-${++sequence}`
  const payload = buildGameInfoUpdatePayload({ ...saved, configurationRevision: 4 }, schedule, false)

  const first = prepareGameInfoSave(payload, null, createId)
  const retry = prepareGameInfoSave(payload, first.operation, createId)
  const changed = prepareGameInfoSave({ ...payload, end: payload.end + 1_000 }, retry.operation, createId)

  assert.equal(first.payload.operationId, 'operation-1')
  assert.equal(first.payload.configurationRevision, 4)
  assert.equal(retry.operation, first.operation)
  assert.equal(retry.payload.operationId, 'operation-1')
  assert.equal(changed.payload.operationId, 'operation-2')
  assert.notEqual(changed.operation, first.operation)
})
