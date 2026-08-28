import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { NoticeType, type GameNotice } from '../Api'
import { currentListSnapshotRows } from './LatestRequest'
import { MAX_GAME_NOTICE_ROWS, mergeGameNotices, receiveGameNotice } from './NoticeFeed'

const notice = (id: number, type = NoticeType.FirstBlood): GameNotice => ({
  id,
  time: 1_788_000_000_000 + id,
  type,
  values: [`notice-${id}`],
})

test('five thousand notice pushes retain only the newest one hundred unique ids', () => {
  let live: GameNotice[] = []
  for (let id = 1; id <= 5_000; id += 1) {
    const received = receiveGameNotice(notice(id), live, [])
    assert.equal(received.accepted, true)
    live = received.rows
  }

  assert.equal(live.length, MAX_GAME_NOTICE_ROWS)
  assert.equal(live[0].id, 5_000)
  assert.equal(live.at(-1)?.id, 4_901)
  assert.equal(new Set(live.map(({ id }) => id)).size, MAX_GAME_NOTICE_ROWS)
})

test('a duplicate notice from either the socket or HTTP snapshot is rejected before toast side effects', () => {
  const pushed = receiveGameNotice(notice(9), [notice(9)], [])
  assert.equal(pushed.accepted, false)
  assert.equal(pushed.rows.length, 1)

  const backfilled = receiveGameNotice(notice(10), [], [notice(10)])
  assert.equal(backfilled.accepted, false)

  const source = readFileSync('src/components/GameNoticePanel.tsx', 'utf8')
  const guard = source.indexOf('if (!received.accepted) return')
  const toast = source.indexOf('showNotification({', guard)
  assert.ok(guard >= 0 && toast > guard, 'duplicate rejection must precede every notice toast')
  assert.match(source, /mergeGameNotices\(liveNotices, notices \?\? \[\]\)/)
  assert.match(source, /visibleNotices = filteredNotices\.slice\(0, MAX_GAME_NOTICE_ROWS\)/)
})

test('an organizer notice survives one hundred newer live system notices', () => {
  const live = Array.from({ length: MAX_GAME_NOTICE_ROWS }, (_, index) => notice(index + 1))
  const organizer = notice(1_000, NoticeType.Normal)
  organizer.time = live[0].time - 1

  const merged = mergeGameNotices(live, [live[0], organizer])

  assert.equal(merged.length, MAX_GAME_NOTICE_ROWS)
  assert.equal(merged[0], organizer)
  assert.ok(merged.some(({ id }) => id === organizer.id))
  assert.equal(new Set(merged.map(({ id }) => id)).size, MAX_GAME_NOTICE_ROWS)
})

test('a game-scope transition hides the previous game buffer synchronously', () => {
  const gameOneBuffer = { scope: '1', rows: [notice(1)] }
  assert.deepEqual(currentListSnapshotRows('1', gameOneBuffer), gameOneBuffer.rows)
  assert.equal(currentListSnapshotRows('2', gameOneBuffer), undefined)

  const source = readFileSync('src/components/GameNoticePanel.tsx', 'utf8')
  assert.match(source, /currentListSnapshotRows\(noticeScope, newNotices\.current\)/)
  assert.match(source, /currentListSnapshotRows\(noticeScope, noticeSnapshotRows\.current\)/)
})
