import dayjs from 'dayjs'
import assert from 'node:assert/strict'
import test from 'node:test'
import { GameStatus } from '../components/GameCard'
import { getGameDurationMinutes, getGameStatus } from './useGame'

test('sub-minute event progress is finite at every boundary', () => {
  const start = 2_000_000_000_000
  const end = start + 30_000
  const event = { start, end }

  const atStart = getGameStatus(event, dayjs(start))
  assert.equal(atStart.status, GameStatus.OnGoing)
  assert.equal(atStart.progress, 0)
  assert.equal(atStart.total, 0.5)

  const midway = getGameStatus(event, dayjs(start + 15_000))
  assert.equal(midway.progress, 50)
  assert.ok(Number.isFinite(midway.progress))

  const atEnd = getGameStatus(event, dayjs(end))
  assert.equal(atEnd.status, GameStatus.Ended)
  assert.equal(atEnd.progress, 100)
})

test('malformed event windows never expose NaN or out-of-range progress', () => {
  const now = dayjs(2_000_000_000_000)
  const cases = [undefined, {}, { start: Number.NaN, end: 2 }, { start: 3, end: 2 }, { start: 2, end: 2 }]

  for (const event of cases) {
    const status = getGameStatus(event, now)
    assert.equal(status.status, GameStatus.Coming)
    assert.equal(status.progress, 0)
    assert.ok(Number.isFinite(status.progress))
  }
})

test('live duration uses the same corrected clock as lifecycle status', () => {
  const start = 2_000_000_000_000
  const end = start + 60 * 60_000
  const correctedNow = dayjs(start + 10 * 60_000)
  const rawBrowserNow = dayjs(end + 60 * 60_000)
  const projection = getGameStatus({ start, end }, correctedNow)

  assert.equal(projection.status, GameStatus.OnGoing)
  assert.equal(getGameDurationMinutes(projection.status, projection.startTime, projection.endTime, correctedNow), 50)
  assert.equal(getGameDurationMinutes(projection.status, projection.startTime, projection.endTime, rawBrowserNow), 0)
})
