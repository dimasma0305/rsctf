import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import type { FlagEgressEventModel } from '@Api'
import {
  currentFlagEgressBuffer,
  formatFlagEgressAge,
  flagEgressMatchesSearch,
  flagEgressPushIsCurrent,
  flagEgressSnapshotIsCurrent,
  mergeFlagEgressRows,
  normalizeFlagEgressSearch,
  rebaseFlagEgressRows,
} from './FlagEgressFeed'
import { LatestRequest } from './LatestRequest'

const event = (
  id: number,
  cursor: number,
  gameId = 1,
  overrides: Partial<FlagEgressEventModel> = {}
): FlagEgressEventModel => ({
  id,
  cursor,
  gameId,
  participationId: id,
  challengeId: id,
  teamName: `team-${id}`,
  challengeTitle: `challenge-${id}`,
  remoteIp: `192.0.2.${id % 255}`,
  remotePort: 0,
  hitCount: cursor,
  firstSeenUtc: cursor,
  lastSeenUtc: cursor,
  ...overrides,
})

test('sustained Flag Egress updates stay deduplicated and hard-capped', () => {
  let buffered: FlagEgressEventModel[] = []
  for (let cursor = 1; cursor <= 5_000; cursor += 1) {
    buffered = mergeFlagEgressRows([event(cursor % 137, cursor)], buffered, 200)
  }

  assert.equal(buffered.length, 137)
  assert.equal(new Set(buffered.map(({ id }) => id)).size, buffered.length)
  assert.equal(buffered[0].cursor, 5_000)
  assert.equal(mergeFlagEgressRows([event(999, 9_999)], buffered, 50).length, 50)
})

test('newer cursors defeat stale HTTP states while newer snapshots heal missed pushes', () => {
  const live = event(7, 20, 1, { hitCount: 2 })
  const staleHttp = event(7, 10, 1, { hitCount: 1 })
  assert.deepEqual(mergeFlagEgressRows([staleHttp], [live], 10), [live])

  const healedHttp = event(7, 30, 1, { hitCount: 3 })
  assert.deepEqual(mergeFlagEgressRows([healedHttp], [live], 10), [healedHttp])
  assert.deepEqual(rebaseFlagEgressRows([live, healedHttp], 20), [healedHttp])
})

test('viewer/game scope changes hide game A immediately and reject its late page', async () => {
  const gameA = JSON.stringify(['viewer-a', 1])
  const gameB = JSON.stringify(['viewer-a', 2])
  const bufferedA = [event(1, 1, 1)]
  assert.deepEqual(currentFlagEgressBuffer(gameB, gameA, bufferedA), [])
  assert.equal(flagEgressPushIsCurrent(gameB, gameA, 1, 1), false)
  assert.equal(flagEgressPushIsCurrent(gameB, gameB, 1, 2), false)
  assert.equal(flagEgressPushIsCurrent(gameB, gameB, 2, 2), true)

  const requests = new LatestRequest()
  let finishA!: (rows: FlagEgressEventModel[]) => void
  const pendingA = new Promise<FlagEgressEventModel[]>((resolve) => {
    finishA = resolve
  })
  const oldRequest = requests.run(() => pendingA)
  const currentRequest = requests.run(async () => [event(2, 2, 2)])
  finishA(bufferedA)
  assert.equal(await oldRequest, undefined)
  const current = await currentRequest
  assert.equal(current?.[0].gameId, 2)
  assert.equal(flagEgressSnapshotIsCurrent(gameB, gameA, 2, 1), false)
  assert.equal(flagEgressSnapshotIsCurrent(gameB, gameB, 2, 2), true)
})

test('active search applies to live rows by team, challenge, and remote IP', () => {
  const row = event(3, 3, 1, {
    teamName: 'Red Pandas',
    challengeTitle: 'Heap School',
    remoteIp: '203.0.113.44',
  })
  assert.equal(flagEgressMatchesSearch(row, 'PANDAS'), true)
  assert.equal(flagEgressMatchesSearch(row, 'heap'), true)
  assert.equal(flagEgressMatchesSearch(row, '113.44'), true)
  assert.equal(flagEgressMatchesSearch(row, '  RED   TEAM  '), false)
  assert.equal(flagEgressMatchesSearch(row, 'blue'), false)
})

test('live Flag Egress search mirrors server whitespace and scalar bounds', () => {
  assert.equal(normalizeFlagEgressSearch('  ReD   Team  '), 'red team')
  assert.equal(normalizeFlagEgressSearch('x'.repeat(128) + 'not-inspected'), 'x'.repeat(128))
  assert.equal(
    flagEgressMatchesSearch(event(9, 9, 1, { teamName: 'Red Team' }), '  ReD   Team  '),
    true
  )
  assert.equal(
    flagEgressMatchesSearch(event(9, 9, 1, { teamName: 'x'.repeat(128) }), 'x'.repeat(128) + 'suffix'),
    true
  )
})

test('relative timestamps initialize their own Day.js plugin', () => {
  assert.match(formatFlagEgressAge(Date.now() - 60_000, 'en'), /minute/)
})

test('Flag Egress page respects reduced motion when resetting its viewport', () => {
  const source = readFileSync('src/pages/admin/games/[id]/FlagEgress.tsx', 'utf8')
  assert.match(source, /useReducedMotion\(\)/)
  assert.match(source, /behavior: reducedMotion \? 'auto' : 'smooth'/)
})
