import assert from 'node:assert/strict'
import test from 'node:test'
import type { GameInfoModel } from '@Api'
import { buildGameInfoUpdatePayload, gameInfoDraftChanged } from './gameInfoDraft'

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
  const baseline = buildGameInfoUpdatePayload({ ...saved, serverTime: 1_000 }, schedule, false)
  const current = buildGameInfoUpdatePayload({ ...saved, serverTime: 2_000 }, schedule, false)

  assert.equal(gameInfoDraftChanged(current, baseline), false)
  assert.equal('serverTime' in current, false)
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
