import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import type { LogMessageModel } from '../Api'
import {
  adminLogFilterScope,
  adminLogIdentity,
  adminLogMatchesQuery,
  adminLogQueryReducer,
  adminLogQueryScope,
  boundAdminLogRows,
  compareAdminLogsNewestFirst,
  MAX_BUFFERED_ADMIN_LOGS,
  normalizeAdminLogSearch,
  receiveAdminLog,
} from './AdminLogFeed'
import { prependUniqueBoundedRow } from './FeedReconciliation'
import { currentListSnapshotRows } from './LatestRequest'

const log = (id: number, overrides: Partial<LogMessageModel> = {}): LogMessageModel => ({
  id,
  time: 1_788_000_000_000 + id,
  level: 'Information',
  name: 'fixture-admin',
  msg: 'updated event settings',
  ip: '192.0.2.10',
  status: 'Success',
  fingerprint: 'browser-alpha',
  ...overrides,
})

test('level and debounced-search commits reset the page atomically', () => {
  const pageSeven = { level: 'Information', page: 7, search: '' }
  const errorQuery = adminLogQueryReducer(pageSeven, { type: 'level', level: 'Error' })
  assert.deepEqual(errorQuery, { level: 'Error', page: 1, search: '' })
  assert.equal(adminLogQueryScope(errorQuery), JSON.stringify(['Error', 1, '']))

  const searched = adminLogQueryReducer({ ...errorQuery, page: 4 }, { type: 'search', search: 'needle' })
  assert.deepEqual(searched, { level: 'Error', page: 1, search: 'needle' })
  assert.equal(
    adminLogQueryReducer(searched, { type: 'search', search: 'needle' }),
    searched,
    'an unchanged committed filter must not schedule another request'
  )
})

test('live admin logs honor level and every server-supported search field', () => {
  const base = { level: 'Information', page: 1, search: '' }
  assert.equal(adminLogMatchesQuery(log(1), base), true)
  assert.equal(adminLogMatchesQuery(log(1), { ...base, level: 'Error' }), false)

  for (const search of ['FIXTURE-ADMIN', 'event settings', '192.0.2', 'BROWSER-ALPHA']) {
    assert.equal(adminLogMatchesQuery(log(1), { ...base, search }), true, search)
  }
  assert.equal(adminLogMatchesQuery(log(1), { ...base, search: 'not-present' }), false)
})

test('admin log search is literal, case-folded, and bounded like the server', () => {
  const base = { level: 'Information', page: 1, search: '' }
  assert.equal(normalizeAdminLogSearch('  ERROR%_  '), 'error%_')
  assert.equal(normalizeAdminLogSearch('x'.repeat(128) + 'suffix'), 'x'.repeat(128))
  assert.equal(adminLogMatchesQuery(log(1, { msg: 'literal % marker' }), { ...base, search: '%' }), true)
  assert.equal(adminLogMatchesQuery(log(1), { ...base, search: '%' }), false)
})

test('admin logs use the stable id to break equal-timestamp ordering ties', () => {
  const rows = [log(2, { time: 100 }), log(4, { time: 99 }), log(3, { time: 100 })]
  rows.sort(compareAdminLogsNewestFirst)
  assert.deepEqual(
    rows.map(({ id }) => id),
    [3, 2, 4]
  )
})

test('five thousand admin pushes retain fifty unique stable ids', () => {
  let buffered: LogMessageModel[] = []
  for (let id = 1; id <= 5_000; id += 1) {
    buffered = prependUniqueBoundedRow(log(id), buffered, MAX_BUFFERED_ADMIN_LOGS, adminLogIdentity)
  }
  assert.equal(buffered.length, MAX_BUFFERED_ADMIN_LOGS)
  assert.equal(buffered[0].id, 5_000)
  assert.equal(buffered.at(-1)?.id, 4_951)

  buffered = prependUniqueBoundedRow(
    log(5_000, { msg: 'refreshed' }),
    buffered,
    MAX_BUFFERED_ADMIN_LOGS,
    adminLogIdentity
  )
  assert.equal(buffered.filter(({ id }) => id === 5_000).length, 1)
  assert.equal(buffered[0].msg, 'refreshed')
})

test('more than fifty unrelated pushes cannot evict a matching live row', () => {
  const warningQuery = { level: 'Warning', page: 1, search: 'keep-me' }
  let buffered = receiveAdminLog(log(1, { level: 'Warning', msg: 'keep-me' }), [], warningQuery).rows

  for (let id = 2; id <= 102; id += 1) {
    const received = receiveAdminLog(log(id, { level: 'Information', msg: 'unrelated' }), buffered, warningQuery)
    assert.equal(received.accepted, false)
    assert.equal(received.rows, buffered, 'unrelated traffic must not churn the scoped buffer')
    buffered = received.rows
  }

  assert.deepEqual(
    buffered.map(({ id }) => id),
    [1]
  )
})

test('admin live buffers are filter-scoped, page-stable, and bounded after filtering', () => {
  const warningQuery = { level: 'Warning', page: 1, search: '' }
  const errorQuery = { level: 'Error', page: 1, search: 'needle' }
  const warningBuffer = {
    scope: adminLogFilterScope(warningQuery),
    rows: [log(1, { level: 'Warning' })],
  }

  assert.equal(adminLogFilterScope({ ...warningQuery, page: 7 }), warningBuffer.scope)
  assert.equal(currentListSnapshotRows(adminLogFilterScope(errorQuery), warningBuffer), undefined)

  const mixed = [
    ...Array.from({ length: 75 }, (_, index) => log(index + 10, { level: 'Information' })),
    log(2, { level: 'Error', msg: 'needle' }),
  ]
  assert.deepEqual(
    boundAdminLogRows(mixed, errorQuery).map(({ id }) => id),
    [2],
    'the cap must be applied after filtering'
  )

  const received = receiveAdminLog(log(3, { level: 'Error', msg: 'needle' }), warningBuffer.rows, errorQuery)
  assert.equal(received.accepted, true)
  assert.deepEqual(
    received.rows.map(({ id }) => id),
    [3]
  )
})

test('the Logs callsite owns one scoped latest request and no separate page-reset effect', () => {
  const source = readFileSync('src/pages/admin/Logs.tsx', 'utf8')
  assert.match(source, /useReducer\(adminLogQueryReducer/)
  assert.match(source, /new LatestListRequest<LogMessageModel>\(\)/)
  assert.match(source, /newLogs = useRef<ListSnapshot<LogMessageModel>>/)
  assert.match(source, /currentListSnapshotRows\(queryScope, logSnapshot\)/)
  assert.match(source, /currentListSnapshotRows\(liveScope, newLogs\.current\)/)
  assert.match(source, /if \(!queryReady\) return/)
  assert.match(source, /receiveAdminLog\(message, liveRows, query\)/)
  assert.doesNotMatch(source, /useEffect\(\(\) => \{\s*setPage\(1\)/)
})
