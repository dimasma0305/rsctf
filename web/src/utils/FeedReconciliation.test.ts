import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { mergeUniqueRows, prependBoundedRow, prependUniqueBoundedRow, reconcileLiveRows } from './FeedReconciliation'

interface DisplayedLog {
  id: number
  marker: string
  time: number
  name: string
  level: string
  ip: string
  msg: string
  status: string
  fingerprint: string
}

const duplicateLog = (marker: string, id: number): DisplayedLog => ({
  id,
  marker,
  time: 1_788_000_000_123,
  name: 'admin',
  level: 'Information',
  ip: '192.0.2.10',
  msg: 'updated event settings',
  status: 'Success',
  fingerprint: 'same-browser',
})

const displayedLogIdentity = (item: DisplayedLog) => item.id

test('stable audit ids reconcile pushed rows without collapsing identical authoritative records', () => {
  const authoritative = [duplicateLog('snapshot-one', 1), duplicateLog('snapshot-two', 2)]
  const pushed = [duplicateLog('push-one', 1), duplicateLog('push-two', 2), duplicateLog('push-three', 3)]

  const unreconciled = reconcileLiveRows(pushed, authoritative, displayedLogIdentity)
  assert.deepEqual(
    unreconciled.map(({ id }) => id),
    [3]
  )

  const visible = mergeUniqueRows(unreconciled, authoritative, displayedLogIdentity, 50)
  assert.equal(visible.length, 3)
  assert.deepEqual(
    visible.filter(({ marker }) => marker.startsWith('snapshot')).map(({ marker }) => marker),
    ['snapshot-one', 'snapshot-two'],
    'the authoritative page retains both distinct database rows despite their identical display identity'
  )
})

test('five thousand stable-id pushes keep unique live and merged collections hard bounded', () => {
  let buffered: DisplayedLog[] = []
  for (let index = 0; index < 5_000; index += 1) {
    buffered = prependUniqueBoundedRow(duplicateLog(`push-${index}`, index), buffered, 100, displayedLogIdentity)
  }

  assert.equal(buffered.length, 100)
  assert.equal(buffered[0].id, 4_999)
  assert.equal(buffered.at(-1)?.id, 4_900)

  buffered = prependUniqueBoundedRow(duplicateLog('refreshed', 4_999), buffered, 100, displayedLogIdentity)
  assert.equal(buffered.length, 100)
  assert.equal(buffered.filter(({ id }) => id === 4_999).length, 1)
  assert.equal(buffered[0].marker, 'refreshed')

  const merged = mergeUniqueRows(
    buffered,
    Array.from({ length: 100 }, (_, index) => duplicateLog(`snapshot-${index}`, 4_950 - index)),
    displayedLogIdentity,
    100
  )
  assert.equal(merged.length, 100)
  assert.equal(new Set(merged.map(({ id }) => id)).size, 100)

  assert.deepEqual(prependBoundedRow(duplicateLog('ignored', 9_999), buffered, Number.POSITIVE_INFINITY), [])
})

test('the admin Logs page uses stable-id bounded reconciliation and React keys', () => {
  const source = readFileSync('src/pages/admin/Logs.tsx', 'utf8')
  const feedSource = readFileSync('src/utils/AdminLogFeed.ts', 'utf8')

  assert.match(source, /receiveAdminLog\(message, liveRows, query\)/)
  assert.match(feedSource, /prependUniqueBoundedRow\([\s\S]*?MAX_BUFFERED_ADMIN_LOGS/)
  assert.match(source, /mergeUniqueRows\([\s\S]*?MAX_VISIBLE_ADMIN_LOGS/)
  assert.match(source, /key=\{item\.id\}/)
  assert.doesNotMatch(source, /key=\{`\$\{item\.time\}@\$\{i\}`\}/)
})
