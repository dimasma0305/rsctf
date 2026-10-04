import assert from 'node:assert/strict'
import { test } from 'node:test'
import { competitionScheduleChange } from '../pages/admin/games/[id]/gameInfoDraft'

test('ended event extension explicitly confirms competition reopening', () => {
  assert.deepEqual(competitionScheduleChange({ start: 10, end: 20 }, { start: 10, end: 40 }, 30), {
    confirm: true,
    reopening: true,
  })
})

test('live shortening confirms; metadata-only and future edits do not', () => {
  assert.deepEqual(competitionScheduleChange({ start: 10, end: 40 }, { start: 10, end: 35 }, 30), {
    confirm: true,
    reopening: false,
  })
  for (const [saved, requested] of [
    [
      { start: 40, end: 50 },
      { start: 45, end: 60 },
    ],
    [
      { start: 10, end: 20 },
      { start: 10, end: 20, title: 'Renamed' },
    ],
  ]) {
    assert.deepEqual(competitionScheduleChange(saved, requested, 30), { confirm: false, reopening: false })
  }
})
