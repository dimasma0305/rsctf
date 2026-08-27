import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { mergeReconciledRows, prependBoundedRow, reconcileLiveRows } from './FeedReconciliation'

interface DisplayedLog {
  marker: string
  time: number
  name: string
  level: string
  ip: string
  msg: string
  status: string
  fingerprint: string
}

const duplicateLog = (marker: string): DisplayedLog => ({
  marker,
  time: 1_788_000_000_123,
  name: 'admin',
  level: 'Information',
  ip: '192.0.2.10',
  msg: 'updated event settings',
  status: 'Success',
  fingerprint: 'same-browser',
})

const displayedLogIdentity = (item: DisplayedLog) =>
  JSON.stringify([item.time, item.name, item.level, item.ip, item.msg, item.status, item.fingerprint])

test('admin log reconciliation preserves duplicate authoritative records as a multiset', () => {
  const authoritative = [duplicateLog('snapshot-one'), duplicateLog('snapshot-two')]
  const pushed = [duplicateLog('push-one'), duplicateLog('push-two'), duplicateLog('push-three')]

  const unreconciled = reconcileLiveRows(pushed, authoritative, displayedLogIdentity)
  assert.equal(unreconciled.length, 1, 'two snapshot occurrences consume exactly two pushed occurrences')

  const visible = mergeReconciledRows(unreconciled, authoritative, 50)
  assert.equal(visible.length, 3)
  assert.deepEqual(
    visible.filter(({ marker }) => marker.startsWith('snapshot')).map(({ marker }) => marker),
    ['snapshot-one', 'snapshot-two'],
    'the authoritative page retains both distinct database rows despite their identical display identity'
  )
})

test('admin live-log recovery buffers and merged views have explicit hard bounds', () => {
  let buffered: DisplayedLog[] = []
  for (let index = 0; index < 200; index += 1) {
    buffered = prependBoundedRow({ ...duplicateLog(`push-${index}`), time: index }, buffered, 50)
  }

  assert.equal(buffered.length, 50)
  assert.equal(buffered[0].marker, 'push-199')
  assert.equal(buffered.at(-1)?.marker, 'push-150')
  assert.equal(
    mergeReconciledRows(
      buffered,
      Array.from({ length: 50 }, (_, index) => duplicateLog(`s-${index}`)),
      75
    ).length,
    75
  )
  assert.deepEqual(prependBoundedRow(duplicateLog('ignored'), buffered, Number.POSITIVE_INFINITY), [])
})

test('the admin Logs page uses multiset-safe bounded reconciliation instead of Set de-duplication', () => {
  const source = readFileSync('src/pages/admin/Logs.tsx', 'utf8')

  assert.match(source, /prependBoundedRow\(message, newLogs\.current, MAX_BUFFERED_LOGS\)/)
  assert.match(source, /mergeReconciledRows\([\s\S]*?MAX_VISIBLE_LOGS/)
  assert.doesNotMatch(source, /mergeUniqueRows/)
})
