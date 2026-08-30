import assert from 'node:assert/strict'
import test from 'node:test'
import { sameNormalNoticeDraft, type NormalNoticeDraft } from './NoticeDraft'

const scheduled: NormalNoticeDraft = {
  content: 'maintenance window',
  scheduled: true,
  publishAt: 1_788_000_000_000,
}

test('clearing an existing notice schedule is a real mutation', () => {
  assert.equal(sameNormalNoticeDraft(scheduled, { ...scheduled, scheduled: false, publishAt: null }), false)
})

test('an exact schedule is unchanged while a reschedule is not', () => {
  assert.equal(sameNormalNoticeDraft(scheduled, { ...scheduled }), true)
  assert.equal(sameNormalNoticeDraft(scheduled, { ...scheduled, publishAt: scheduled.publishAt + 60_000 }), false)
  assert.equal(sameNormalNoticeDraft(scheduled, { ...scheduled, content: 'updated' }), false)
})
